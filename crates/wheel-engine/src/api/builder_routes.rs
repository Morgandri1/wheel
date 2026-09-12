// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

//! `POST /v1/builder/turns` and the builder's own credential
//! (`docs/proposals/workflow-builder-completion.md` §1).
//!
//! The logic is in `crate::builder`, which is pure apart from the spawn itself. This is the
//! wiring: check the request, take the project's single turn permit, resolve a credential, and
//! stream the child's answer back as Server-Sent Events.
//!
//! Refusals that happen BEFORE anything spawns are ordinary JSON with a status, because they are
//! answers to the request rather than events in a run: 409 `needs_auth` (with what could be
//! designated instead), 403 `policy`, 429 `builder_busy`, 400, 413. Once the stream is open, a
//! failure is an `error` frame — by then the status line has already been sent.

use axum::{
    extract::State,
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use wheel_core::ErrorBody;

use super::{ApiError, ApiResult, AppState};
use crate::builder::{self, CredentialError, Mode, TurnsRequest};

pub async fn turns(State(s): State<AppState>, Json(raw): Json<serde_json::Value>) -> Response {
    match start(&s, raw).await {
        Ok(response) => response,
        Err(refusal) => *refusal,
    }
}

/// A refusal, boxed: a `Response` is a large value to carry in an `Err` on every call.
type Refused = Box<Response>;

fn refuse(response: impl IntoResponse) -> Refused {
    Box::new(response.into_response())
}

async fn start(s: &AppState, raw: serde_json::Value) -> Result<Response, Refused> {
    let request: TurnsRequest = serde_json::from_value(raw).map_err(|e| {
        refuse(ApiError::invalid(format!(
            "not a builder turn request: {e}"
        )))
    })?;
    builder::check_conversation(&request.turns).map_err(|why| refuse(ApiError::invalid(why)))?;

    // Taken before any work: a second turn must be refused, not queued behind the first.
    let permit = s.builder.try_begin().ok_or_else(|| {
        refuse(ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "builder_busy",
            "a builder turn is already running for this project; wait for it to finish",
        ))
    })?;

    let (board, credential) = {
        let conn =
            s.db.lock()
                .map_err(|_| refuse(ApiError::internal("db poisoned")))?;

        let board = match request.mode {
            Mode::Improve => {
                let nodes = board_nodes(&conn).map_err(refuse)?;
                let board = builder::board_for_builder(&nodes);
                let size = serde_json::to_string(&board).map(|s| s.len()).unwrap_or(0);
                if size > builder::MAX_BOARD_BYTES {
                    return Err(refuse(ApiError::new(
                        StatusCode::PAYLOAD_TOO_LARGE,
                        "board_too_large",
                        format!(
                            "this board is {size} bytes; at most {} can be sent to the builder",
                            builder::MAX_BOARD_BYTES
                        ),
                    )));
                }
                Some(board)
            }
            Mode::New => None,
        };

        let credential =
            builder::resolve(&conn, &s.cfg, s.supervisor.vault_key(), request.credential)
                .map_err(|e| Box::new(credential_refusal(&conn, &s.cfg, e)))?;
        (board, credential)
    };

    let input = builder::compose_input(request.mode, &request.turns, board.as_ref());
    let rx = builder::spawn_turn(&s.builder, &s.cfg, permit, credential, input)
        .await
        .map_err(|e| refuse(ApiError::internal(e.to_string())))?;

    let stream = futures_util::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|frame| {
            (
                Ok::<_, std::convert::Infallible>(axum::body::Bytes::from(frame)),
                rx,
            )
        })
    });
    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "text/event-stream; charset=utf-8"),
            // no-transform: a compressing hop that buffers would turn a stream into one late
            // response, which is the whole thing this route exists to avoid.
            (header::CACHE_CONTROL, "no-cache, no-transform"),
            (header::HeaderName::from_static("x-accel-buffering"), "no"),
        ],
        axum::body::Body::from_stream(stream),
    )
        .into_response())
}

fn board_nodes(conn: &rusqlite::Connection) -> Result<Vec<wheel_core::Node>, ApiError> {
    crate::db::board::list(conn).map_err(|e| ApiError::internal(e.to_string()))
}

/// A credential refusal, and — when the answer is "nothing is stored" — what the user could point
/// the builder at instead. A 409 that only says no sends them looking; this one says where to go.
fn credential_refusal(
    conn: &rusqlite::Connection,
    cfg: &crate::config::Config,
    error: CredentialError,
) -> Response {
    let (status, code, message) = match error {
        CredentialError::NeedsAuth(m) => (StatusCode::CONFLICT, "needs_auth", m),
        CredentialError::Invalid(m) => (StatusCode::BAD_REQUEST, "invalid", m),
        CredentialError::Ambiguous(m) => (StatusCode::CONFLICT, "ambiguous_credential", m),
        CredentialError::Policy(m) => (StatusCode::FORBIDDEN, "policy", m),
        CredentialError::Unavailable(m) => (StatusCode::SERVICE_UNAVAILABLE, "config", m),
        CredentialError::Internal(m) => (StatusCode::INTERNAL_SERVER_ERROR, "internal", m),
    };
    let mut body = serde_json::json!(ErrorBody::new(code, message));
    if code == "needs_auth" {
        body["sources"] = builder::candidate_sources(conn, cfg);
    }
    (status, Json(body)).into_response()
}

// --- the builder's own credential ------------------------------------------

#[derive(Debug, Deserialize)]
pub struct StoreCredential {
    #[serde(default)]
    pub api_key: Option<String>,
    /// A durable `claude setup-token` credential, refused unless it really is one — same rule as
    /// an agent's `auth/complete`, for the same reason.
    #[serde(default)]
    pub setup_token: Option<String>,
}

pub async fn credential_status(State(s): State<AppState>) -> Json<serde_json::Value> {
    let kind =
        crate::auth::stored_token_kind(&builder::builder_dir(&s.cfg), wheel_core::Harness::Claude);
    Json(serde_json::json!({ "configured": kind.is_some(), "kind": kind }))
}

pub async fn credential_put(
    State(s): State<AppState>,
    Json(body): Json<StoreCredential>,
) -> ApiResult<Json<serde_json::Value>> {
    let (value, must_be_durable) = match (body.api_key, body.setup_token) {
        (Some(key), None) => (key, false),
        (None, Some(token)) => (token, true),
        _ => {
            return Err(ApiError::invalid(
                "supply exactly one of: api_key (a provider key), or setup_token (from \
                 `claude setup-token`)",
            ))
        }
    };
    let value = value.trim().to_string();
    let kind = crate::auth::classify_token(&value, wheel_core::Harness::Claude);
    if must_be_durable && kind != wheel_core::CredentialKind::OauthToken {
        return Err(ApiError::invalid(
            "that is not a `claude setup-token` credential (expected one starting `sk-ant-oat`); \
             submit a provider key as api_key instead",
        ));
    }
    // The store is gated as well as the spawn. Both, because failing here says why while the
    // credential can still be changed, and failing at spawn is what an agent cannot walk around.
    if s.cfg.harness_auth == crate::config::HarnessAuthPolicy::ApiKeyOnly
        && kind == wheel_core::CredentialKind::OauthToken
    {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "policy",
            "this project is api-key-only: an OAuth-shaped credential is not permitted here, \
             store an API key instead",
        ));
    }

    crate::auth::store_token(
        &builder::builder_dir(&s.cfg),
        &value,
        wheel_core::Harness::Claude,
    )
    .map_err(|e| ApiError::invalid(e.to_string()))?;
    Ok(Json(
        serde_json::json!({ "configured": true, "kind": kind }),
    ))
}

pub async fn credential_delete(State(s): State<AppState>) -> ApiResult<StatusCode> {
    crate::auth::clear_token(&builder::builder_dir(&s.cfg))
        .map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{test_state, test_state_with};
    use crate::builder::Builder;
    use crate::config::HarnessAuthPolicy;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::time::Duration;

    /// The REAL fake harness, not a stub of one: `qa/harness/fake-claude` speaks the same
    /// stream-json protocol as the CLI, so these tests exercise the engine's actual parsing and
    /// spawn path. A stub here would only ever agree with whatever this module already does.
    fn fake_claude() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../qa/harness/fake-claude")
    }

    /// A wrapper that records what the child was ACTUALLY given — argv, environment, the prompt
    /// file and stdin — because every one of those is a property this route has to hold and none
    /// of them is visible from the response.
    fn shim(dir: &Path, extra: &[(&str, &str)]) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(dir).unwrap();
        let exports: String = extra
            .iter()
            .map(|(k, v)| format!("export {k}=\"{v}\"\n"))
            .collect();
        let script = format!(
            "#!/bin/sh\n\
             prev=\"\"\n\
             for a in \"$@\"; do\n\
             \tif [ \"$prev\" = \"--system-prompt-file\" ]; then cp \"$a\" \"{dir}/system-copy.md\"; fi\n\
             \tprev=\"$a\"\n\
             done\n\
             export WHEEL_FAKE_ENV_DUMP=\"{dir}/env.jsonl\"\n\
             export WHEEL_FAKE_TRANSCRIPT=\"{dir}/stdin.txt\"\n\
             export WHEEL_FAKE_STRICT_AUTH=1\n\
             {exports}\
             exec python3 \"{fake}\" \"$@\"\n",
            dir = dir.display(),
            fake = fake_claude().display(),
        );
        let path = dir.join("claude.sh");
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, PermissionsExt::from_mode(0o755)).unwrap();
        path
    }

    struct Harness {
        state: AppState,
        dir: PathBuf,
    }

    impl Drop for Harness {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.dir).ok();
        }
    }

    impl Harness {
        fn new(name: &str) -> Self {
            Self::build(
                name,
                &[],
                Duration::from_secs(30),
                HarnessAuthPolicy::default(),
            )
        }

        fn with_env(name: &str, extra: &[(&str, &str)]) -> Self {
            Self::build(
                name,
                extra,
                Duration::from_secs(30),
                HarnessAuthPolicy::default(),
            )
        }

        fn build(
            name: &str,
            extra: &[(&str, &str)],
            timeout: Duration,
            policy: HarnessAuthPolicy,
        ) -> Self {
            assert!(
                std::process::Command::new("python3")
                    .arg("--version")
                    .output()
                    .is_ok_and(|o| o.status.success()),
                "python3 is required to run the fake harness; a gate that cannot run must fail \
                 rather than look like it passed"
            );
            let mut state = test_state_with(policy, None, |_| {});
            let dir = std::env::temp_dir().join(format!(
                "wheel-builder-route-{name}-{}-{}",
                std::process::id(),
                uuid::Uuid::new_v4()
            ));
            let program = shim(&dir, extra);
            state.builder = Arc::new(Builder::with_program(
                program.display().to_string(),
                timeout,
            ));
            Self { state, dir }
        }

        /// A credential for the builder itself, the way `PUT /v1/builder/credential` stores one.
        fn sign_in(&self, key: &str) {
            crate::auth::store_token(
                &crate::builder::builder_dir(&self.state.cfg),
                key,
                wheel_core::Harness::Claude,
            )
            .unwrap();
        }

        async fn turn(&self, body: serde_json::Value) -> (StatusCode, String) {
            let response = turns(State(self.state.clone()), Json(body)).await;
            let status = response.status();
            let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
                .await
                .unwrap();
            (status, String::from_utf8_lossy(&bytes).into_owned())
        }

        fn recorded(&self, file: &str) -> String {
            std::fs::read_to_string(self.dir.join(file)).unwrap_or_default()
        }

        /// What the child was spawned with, as the fake recorded it.
        fn spawn_record(&self) -> serde_json::Value {
            let dump = self.recorded("env.jsonl");
            let line = dump
                .lines()
                .next()
                .unwrap_or_else(|| panic!("the fake recorded no spawn at all; dump was {dump:?}"));
            serde_json::from_str(line).unwrap()
        }
    }

    /// SSE text as (event, data) pairs.
    fn frames(body: &str) -> Vec<(String, serde_json::Value)> {
        body.split("\n\n")
            .filter(|block| !block.trim().is_empty())
            .map(|block| {
                let mut event = String::new();
                let mut data = String::new();
                for line in block.lines() {
                    if let Some(rest) = line.strip_prefix("event: ") {
                        event = rest.to_string();
                    } else if let Some(rest) = line.strip_prefix("data: ") {
                        data.push_str(rest);
                    }
                }
                (
                    event,
                    serde_json::from_str(&data).unwrap_or(serde_json::Value::Null),
                )
            })
            .collect()
    }

    fn user_turn(text: &str) -> serde_json::Value {
        serde_json::json!({"mode": "new", "turns": [{"role": "user", "text": text}]})
    }

    #[tokio::test]
    async fn a_turn_streams_its_answer_and_ends_with_the_board_it_proposed() {
        // Steered by env, not by a directive in the message: the route escapes user text before
        // the child sees it, which is exactly what `nothing_quoted_can_close_its_own_frame` pins.
        let h = Harness::with_env("stream", &[("WHEEL_FAKE_BOARD", "1")]);
        h.sign_in("sk-ant-api03-builderkey");

        let (status, body) = h.turn(user_turn("design me a researcher")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let frames = frames(&body);

        let deltas: Vec<&str> = frames
            .iter()
            .filter(|(e, _)| e == "delta")
            .filter_map(|(_, d)| d["text"].as_str())
            .collect();
        assert!(!deltas.is_empty(), "the answer never streamed: {body}");

        let (_, done) = frames
            .iter()
            .find(|(e, _)| e == "done")
            .unwrap_or_else(|| panic!("no done frame: {body}"));
        let text = done["text"].as_str().unwrap();
        assert!(text.contains(crate::builder::START_MARKER), "{text}");
        assert_eq!(
            done["boards"], 1,
            "exactly one board is the contract: {text}"
        );
        // The streamed text is the same answer, not a different one.
        assert!(
            text.contains(deltas.concat().trim()) || deltas.concat().contains(text.trim()),
            "the deltas and the final text disagree"
        );
    }

    /// §5b. argv is world-readable across uids, so the prompt travels as a path and the
    /// conversation on stdin. Asserted against what the child actually received.
    #[tokio::test]
    async fn no_prompt_or_conversation_text_ever_reaches_the_command_line() {
        let h = Harness::new("argv");
        h.sign_in("sk-ant-api03-builderkey");
        let secret_ask = "a-very-distinctive-phrase-the-user-typed";

        let (status, _) = h.turn(user_turn(secret_ask)).await;
        assert_eq!(status, StatusCode::OK);

        let record = h.spawn_record();
        let argv: Vec<String> = serde_json::from_value(record["argv"].clone()).unwrap();
        assert!(
            argv.contains(&"--system-prompt-file".to_string()),
            "{argv:?}"
        );
        for arg in &argv {
            assert!(
                !arg.contains(secret_ask),
                "the user's words were on argv: {argv:?}"
            );
            assert!(
                !arg.contains("Workflow Builder"),
                "the prompt was on argv: {argv:?}"
            );
        }
        // The prompt really did arrive, by file, byte for byte.
        assert_eq!(h.recorded("system-copy.md"), crate::builder::PROMPT);
        // ...and the conversation arrived on stdin.
        assert!(
            h.recorded("stdin.txt").contains(secret_ask),
            "stdin was empty"
        );
    }

    /// One credential variable, carrying the project's own key, and nothing that would let the
    /// builder reach the board or the engine.
    #[tokio::test]
    async fn the_child_gets_one_credential_and_no_way_back_into_the_engine() {
        let h = Harness::new("env");
        h.sign_in("sk-ant-api03-builderkey");
        let (status, body) = h.turn(user_turn("hello")).await;
        // WHEEL_FAKE_STRICT_AUTH=1: without a credential the fake exits needs_auth, so a 200 with
        // a done frame is itself proof the key arrived.
        assert_eq!(status, StatusCode::OK);
        assert!(frames(&body).iter().any(|(e, _)| e == "done"), "{body}");

        let record = h.spawn_record();
        let vars: Vec<String> =
            serde_json::from_value(record["credential_vars_set"].clone()).unwrap();
        assert_eq!(
            vars,
            vec!["ANTHROPIC_API_KEY".to_string()],
            "exactly one credential variable, or which one wins is the harness's guess"
        );
        assert_eq!(
            record["credentials"]["ANTHROPIC_API_KEY"]["class"], "api_key",
            "the key was routed as the kind it is"
        );

        let names: Vec<String> = serde_json::from_value(record["env_names"].clone()).unwrap();
        for forbidden in [
            "WHEEL_TOKEN_FILE",
            "WHEEL_TOKEN",
            "WHEEL_ENGINE_URL",
            "WHEEL_NODE",
        ] {
            assert!(
                !names.iter().any(|n| n == forbidden),
                "the builder was handed {forbidden}, which is a way onto the board: {names:?}"
            );
        }
        // The engine's own secret must not be in that environment under ANY name (F015/037): a
        // rename would pass a name check, so the values are compared by digest.
        let digests = record["env_digests"].as_object().unwrap();
        let engine_secret = {
            use sha2::Digest;
            format!(
                "{:x}",
                sha2::Sha256::digest(h.state.cfg.engine_secret.as_bytes())
            )
        };
        assert!(
            !digests
                .values()
                .any(|d| d == &serde_json::json!(engine_secret)),
            "the engine secret reached the builder's child"
        );
    }

    #[tokio::test]
    async fn improving_shows_the_builder_the_board_without_its_secrets() {
        let h = Harness::new("improve");
        h.sign_in("sk-ant-api03-builderkey");
        {
            let conn = h.state.db.lock().unwrap();
            let mcp = wheel_core::Node::new(
                uuid::Uuid::new_v4(),
                "server".parse().unwrap(),
                wheel_core::Position::default(),
                wheel_core::NodeConfig::Mcp(wheel_core::McpConfig::Stdio {
                    command: "run".into(),
                    args: None,
                    env: Some(
                        [("API_TOKEN".to_string(), "sk-super-secret".to_string())]
                            .into_iter()
                            .collect(),
                    ),
                }),
            );
            crate::db::board::create(&conn, &mcp).unwrap();
        }

        let (status, body) = h
            .turn(serde_json::json!({
                "mode": "improve",
                "turns": [{"role": "user", "text": "add a table"}]
            }))
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");

        let stdin = h.recorded("stdin.txt");
        assert!(
            stdin.contains("current_board"),
            "the board was not attached: {stdin}"
        );
        assert!(
            stdin.contains("server"),
            "the node names are what the builder reasons about"
        );
        assert!(
            !stdin.contains("sk-super-secret"),
            "an mcp env value reached the model: {stdin}"
        );
        // `<` is written as \u003c so the framed board stays valid JSON, so the marker is matched
        // by its distinctive word rather than by a spelling that depends on the escaping.
        assert!(stdin.contains("redacted"), "{stdin}");
    }

    #[tokio::test]
    async fn with_no_credential_the_answer_says_what_to_point_it_at() {
        let h = Harness::new("needsauth");
        let (status, body) = h.turn(user_turn("hello")).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        let answer: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(answer["error"]["code"], "needs_auth", "{body}");
        // The lists may be empty on a bare project, but the shape is what the UI offers from.
        assert!(answer["sources"]["agents"].is_array(), "{body}");
        assert!(answer["sources"]["vaults"].is_array(), "{body}");
    }

    /// A credential that is stored but rejected is not the same as none stored: it arrives once
    /// the stream is open, so it is an `error` frame rather than a status.
    #[tokio::test]
    async fn a_rejected_credential_arrives_as_a_needs_auth_frame() {
        let h = Harness::with_env("rejected", &[("WHEEL_FAKE_AUTH", "needs_auth")]);
        h.sign_in("sk-ant-api03-stale");
        let (status, body) = h.turn(user_turn("hello")).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "the stream opened before the child failed"
        );
        let (_, data) = frames(&body)
            .into_iter()
            .find(|(e, _)| e == "error")
            .unwrap_or_else(|| panic!("no error frame: {body}"));
        assert_eq!(data["code"], "needs_auth", "{data}");
    }

    #[tokio::test]
    async fn a_failed_turn_reports_the_failure_rather_than_an_empty_answer() {
        let h = Harness::new("error");
        let script = h.dir.join("turns.jsonl");
        std::fs::write(
            &script,
            "{\"is_error\": true, \"error\": \"the model fell over\"}\n",
        )
        .unwrap();
        let h = Harness::with_env(
            "error-run",
            &[("WHEEL_FAKE_SCRIPT", &script.display().to_string())],
        );
        h.sign_in("sk-ant-api03-builderkey");
        let (_, body) = h.turn(user_turn("design something")).await;
        let (_, data) = frames(&body)
            .into_iter()
            .find(|(e, _)| e == "error")
            .unwrap_or_else(|| panic!("no error frame: {body}"));
        assert_eq!(data["code"], "builder_error", "{data}");
        assert!(
            data["message"].as_str().unwrap().contains("fell over"),
            "the harness's own words are what say why: {data}"
        );
    }

    /// Every turn spends the user's money, so a client that loops must be refused rather than
    /// queued: the engine is the only place that can hold that line.
    #[tokio::test]
    async fn only_one_turn_runs_at_a_time() {
        let h = Harness::new("busy");
        h.sign_in("sk-ant-api03-builderkey");
        let held = h.state.builder.try_begin().expect("the first permit");

        let (status, body) = h.turn(user_turn("hello")).await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{body}");
        assert!(body.contains("builder_busy"), "{body}");

        drop(held);
        let (status, _) = h.turn(user_turn("hello again")).await;
        assert_eq!(status, StatusCode::OK, "the permit must come back");
    }

    #[tokio::test]
    async fn a_turn_that_overruns_is_stopped_and_says_so() {
        let scripts = Harness::new("timeout-script");
        let script = scripts.dir.join("turns.jsonl");
        std::fs::write(&script, "{\"sleep\": 30, \"reply\": \"too late\"}\n").unwrap();
        let h = Harness::build(
            "timeout",
            &[("WHEEL_FAKE_SCRIPT", &script.display().to_string())],
            Duration::from_secs(1),
            HarnessAuthPolicy::default(),
        );
        h.sign_in("sk-ant-api03-builderkey");
        let (_, body) = h.turn(user_turn("design something")).await;
        let (_, data) = frames(&body)
            .into_iter()
            .find(|(e, _)| e == "error")
            .unwrap_or_else(|| panic!("no error frame: {body}"));
        assert_eq!(data["code"], "timeout", "{data}");
    }

    #[tokio::test]
    async fn a_request_with_nothing_to_answer_is_refused_before_anything_spawns() {
        let h = Harness::new("invalid");
        h.sign_in("sk-ant-api03-builderkey");
        for body in [
            serde_json::json!({"mode": "new", "turns": []}),
            serde_json::json!({"mode": "new", "turns": [{"role": "builder", "text": "hi"}]}),
            serde_json::json!({"mode": "sideways", "turns": [{"role": "user", "text": "hi"}]}),
        ] {
            let (status, answer) = h.turn(body.clone()).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{body} -> {answer}");
        }
        assert_eq!(
            h.recorded("env.jsonl"),
            "",
            "a refused request must not have spawned a child"
        );
    }

    // --- the builder's own credential --------------------------------------

    #[tokio::test]
    async fn the_builders_credential_can_be_stored_read_and_cleared() {
        let h = Harness::new("credential");
        let status = credential_status(State(h.state.clone())).await;
        assert_eq!(status.0["configured"], false);

        let _stored = credential_put(
            State(h.state.clone()),
            Json(StoreCredential {
                api_key: Some("sk-ant-api03-typed".into()),
                setup_token: None,
            }),
        )
        .await
        .expect("stored");
        let status = credential_status(State(h.state.clone())).await;
        assert_eq!(status.0["configured"], true);
        assert_eq!(status.0["kind"], "api_key");

        credential_delete(State(h.state.clone()))
            .await
            .expect("cleared");
        let status = credential_status(State(h.state.clone())).await;
        assert_eq!(status.0["configured"], false);
    }

    #[tokio::test]
    async fn a_setup_token_field_only_accepts_a_setup_token() {
        let h = Harness::new("setuptoken");
        let refused = credential_put(
            State(h.state.clone()),
            Json(StoreCredential {
                api_key: None,
                setup_token: Some("sk-ant-api03-not-a-setup-token".into()),
            }),
        )
        .await
        .expect_err("a provider key is not a setup token");
        assert_eq!(refused.0, StatusCode::BAD_REQUEST);

        // Neither field, or both, is a caller that has not decided.
        assert!(credential_put(
            State(h.state.clone()),
            Json(StoreCredential {
                api_key: None,
                setup_token: None
            })
        )
        .await
        .is_err());
        assert!(credential_put(
            State(h.state.clone()),
            Json(StoreCredential {
                api_key: Some("sk-ant-api03-a".into()),
                setup_token: Some("sk-ant-oat01-b".into()),
            })
        )
        .await
        .is_err());
    }

    /// The store is gated as well as the spawn: refusing here says why while the credential can
    /// still be changed, instead of at the moment someone wants an answer.
    #[tokio::test]
    async fn an_api_key_only_project_will_not_store_an_oauth_credential_for_the_builder() {
        let state = test_state_with(HarnessAuthPolicy::ApiKeyOnly, None, |_| {});
        let refused = credential_put(
            State(state.clone()),
            Json(StoreCredential {
                api_key: None,
                setup_token: Some("sk-ant-oat01-durable".into()),
            }),
        )
        .await
        .expect_err("api-key-only refuses an OAuth credential");
        assert_eq!(refused.0, StatusCode::FORBIDDEN);
        assert_eq!(refused.1, "policy");

        // An API key on the same deployment still works, or the policy would be an outage.
        let _stored = credential_put(
            State(state.clone()),
            Json(StoreCredential {
                api_key: Some("sk-ant-api03-fine".into()),
                setup_token: None,
            }),
        )
        .await
        .expect("an api key is what this deployment wants");
        std::fs::remove_dir_all(&state.cfg.data_dir).ok();
    }

    #[tokio::test]
    async fn the_engine_route_table_reaches_these_handlers() {
        // ADVERSARY 025's shape: a handler nobody routed compiles, unit-tests green, and 404s in
        // the client's hands.
        let router = include_str!("mod.rs");
        for handler in [
            "builder_routes::turns",
            "builder_routes::credential_status",
            "builder_routes::credential_put",
            "builder_routes::credential_delete",
        ] {
            assert!(
                router.contains(handler),
                "{handler} exists but nothing routes it"
            );
        }
        let _ = test_state();
    }
}
