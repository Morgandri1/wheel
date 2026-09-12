// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

//! Agent lifecycle and messaging routes.
//!
//! `send` deliberately does NOT write to the child. It persists a message and
//! nudges the delivery loop, which is the only stdin writer (§3c#12) — and a
//! message never spawns a process (§3c#13), it enqueues.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use serde::Deserialize;
use uuid::Uuid;
use wheel_core::{
    AgentStatus, MessageReceipt, MessageSender, NodeType, Timestamp, MAX_MESSAGE_BODY,
};

use super::{ApiError, ApiResult, AppState};
use crate::db::{board, messages};

#[derive(Debug, Deserialize)]
pub struct SendBody {
    pub body: String,
    #[serde(default)]
    pub reply_to: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
pub struct LogQuery {
    #[serde(default)]
    pub since: Option<i64>,
    #[serde(default)]
    pub stream: Option<String>,
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub struct InboxQuery {
    #[serde(default)]
    pub since: Option<String>,
    #[serde(default)]
    pub limit: Option<u32>,
}

/// Confirm the node exists and really is an agent, so lifecycle routes cannot
/// be aimed at a ctx or a vault.
fn require_agent(s: &AppState, id: Uuid) -> ApiResult<()> {
    let conn = s.db.lock().map_err(|_| ApiError::internal("db poisoned"))?;
    let node = board::get(&conn, id)
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| ApiError::not_found(id.to_string()))?;
    if node.node_type() != NodeType::Agent {
        return Err(ApiError::invalid(format!(
            "{} is a {} node, not an agent",
            node.name,
            node.node_type()
        )));
    }
    Ok(())
}

/// `POST /v1/agents/:id/start` — idempotent (§3c#13).
pub async fn start(
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    require_agent(&s, id)?;
    let status = s
        .supervisor
        .start(id)
        .await
        .map_err(|e| ApiError::new(StatusCode::BAD_GATEWAY, "harness_error", e.to_string()))?;

    // Anything queued while the agent was stopped drains now.
    let _ = s.supervisor.deliver(id).await;
    Ok(Json(status_body(&s, id, status)))
}

/// `POST /v1/agents/:id/stop`
pub async fn stop(
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    require_agent(&s, id)?;
    let status = s
        .supervisor
        .stop(id)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(Json(status_body(&s, id, status)))
}

/// `POST /v1/agents/:id/interrupt` (§3c#12, PROTOCOL.md M2) — cancel the turn
/// this agent is in the middle of, without losing its session. `stop` also
/// cancels it, but takes the ability to resume with it; this is the smaller,
/// explicit action for "stop talking, something more important arrived",
/// distinct from `send`, which never interrupts a turn in progress.
pub async fn interrupt(
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    require_agent(&s, id)?;
    let status = s
        .supervisor
        .interrupt(id)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(Json(status_body(&s, id, status)))
}

/// `POST /v1/agents/:id/restart`
pub async fn restart(
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    require_agent(&s, id)?;
    s.supervisor
        .stop(id)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    let status = s
        .supervisor
        .start(id)
        .await
        .map_err(|e| ApiError::new(StatusCode::BAD_GATEWAY, "harness_error", e.to_string()))?;
    let _ = s.supervisor.deliver(id).await;
    Ok(Json(status_body(&s, id, status)))
}

/// `POST /v1/agents/:id/clear`
///
/// Discard the agent's context and rebuild it: a new session with the system
/// prompt and every wired ctx node re-injected. Backs `wheel ctx clear` and is
/// the same path `ephemeral_context` takes after a turn.
pub async fn clear(
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    require_agent(&s, id)?;
    let status = s
        .supervisor
        .clear_context(id)
        .await
        .map_err(|e| ApiError::new(StatusCode::BAD_GATEWAY, "harness_error", e.to_string()))?;
    Ok(Json(status_body(&s, id, status)))
}

/// `POST /v1/agents/:id/send` — user → agent.
///
/// Returns the receipt immediately; delivery is the loop's job. The body limit
/// is checked here as well as in the CLI so a caller gets a clear error rather
/// than discovering the limit by failing (§3c#6).
pub async fn send(
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<SendBody>,
) -> ApiResult<(StatusCode, Json<MessageReceipt>)> {
    require_agent(&s, id)?;
    if body.body.len() > MAX_MESSAGE_BODY {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "too_large",
            format!(
                "message body is {} bytes, limit is {MAX_MESSAGE_BODY}",
                body.body.len()
            ),
        ));
    }

    let msg = {
        let conn = s.db.lock().map_err(|_| ApiError::internal("db poisoned"))?;
        messages::enqueue(&conn, MessageSender::User, id, body.body, body.reply_to)
            .map_err(|e| ApiError::internal(e.to_string()))?
    };

    // Nudge the loop. If the agent is stopped or mid-turn this is a no-op and
    // the message simply waits — it is never dropped and never truncated.
    let _ = s.supervisor.deliver(id).await;

    Ok((StatusCode::ACCEPTED, Json(MessageReceipt::from(&msg))))
}

/// `GET /v1/agents/:id/log?since=&stream=&limit=`
///
/// `stream=transcript` returns the exact bytes written to the child's stdin,
/// on this same route so the UI needs no second subscription.
pub async fn log(
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
    Query(q): Query<LogQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    // An unknown stream is refused rather than ignored. Passing it through to
    // SQL would match no rows and return an EMPTY page, which is the worst
    // outcome: the operator sees a heading with nothing under it and concludes
    // the agent produced no output. A filter that silently does not filter is
    // worse than one that says no.
    // An empty value is treated as no filter, not as an unknown stream: a UI
    // rendering `?stream=${selected}` sends exactly that for its "all" tab,
    // and 400-ing the default view would be a rude way to say "no filter".
    let stream = q.stream.filter(|s| !s.is_empty());
    if let Some(stream) = stream.as_deref() {
        if serde_json::from_value::<wheel_core::LogStream>(serde_json::Value::String(
            stream.to_string(),
        ))
        .is_err()
        {
            return Err(ApiError::invalid(format!(
                "unknown stream {stream:?}; valid streams are stdout, stderr, engine, transcript"
            )));
        }
    }

    let conn = s.db.lock().map_err(|_| ApiError::internal("db poisoned"))?;
    let since = q.since.unwrap_or(0);
    let limit = q.limit.unwrap_or(500).min(10_000) as i64;

    let mut stmt = conn
        .prepare(
            "SELECT seq, stream, at, text FROM logs
             WHERE node_id = ?1 AND seq > ?2 AND (?3 IS NULL OR stream = ?3)
             ORDER BY seq LIMIT ?4",
        )
        .map_err(|e| ApiError::internal(e.to_string()))?;

    let rows = stmt
        .query_map(
            rusqlite::params![id.to_string(), since, stream, limit],
            |r| {
                Ok(serde_json::json!({
                    "node_id": id,
                    "seq": r.get::<_, i64>(0)?,
                    "stream": r.get::<_, String>(1)?,
                    "at": r.get::<_, String>(2)?,
                    "text": r.get::<_, String>(3)?,
                }))
            },
        )
        .map_err(|e| ApiError::internal(e.to_string()))?;

    let lines: Vec<serde_json::Value> = rows
        .collect::<Result<_, _>>()
        .map_err(|e| ApiError::internal(e.to_string()))?;
    let next = lines
        .last()
        .and_then(|l| l["seq"].as_i64())
        .unwrap_or(since);

    Ok(Json(serde_json::json!({ "lines": lines, "next": next })))
}

/// `GET /v1/agents/:id/inbox` — re-read exactly what was delivered (§3c#2).
pub async fn inbox(
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
    Query(q): Query<InboxQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    let conn = s.db.lock().map_err(|_| ApiError::internal("db poisoned"))?;
    let since = q
        .since
        .as_deref()
        .and_then(|t| Timestamp::parse_rfc3339(t).ok());
    let msgs = messages::inbox(&conn, id, since, q.limit.unwrap_or(100).min(1000))
        .map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(Json(serde_json::json!({ "messages": msgs })))
}

/// `GET /v1/agents/:id/inbox/:message_id` — the exact original body.
pub async fn inbox_one(
    State(s): State<AppState>,
    Path((id, message_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<wheel_core::Message>> {
    let conn = s.db.lock().map_err(|_| ApiError::internal("db poisoned"))?;
    let msg = messages::get(&conn, message_id)
        .map_err(|e| ApiError::internal(e.to_string()))?
        .filter(|m| m.to == id)
        .ok_or_else(|| ApiError::not_found(message_id.to_string()))?;
    Ok(Json(msg))
}

fn status_body(s: &AppState, id: Uuid, fallback: AgentStatus) -> serde_json::Value {
    let state =
        s.db.lock()
            .ok()
            .and_then(|conn| board::agent_state(&conn, id).ok());
    match state {
        Some(st) => serde_json::json!({
            "status": st.status.as_str(),
            "session_id": st.session_id,
        }),
        None => serde_json::json!({ "status": fallback.as_str(), "session_id": null }),
    }
}

// --- auth (§4) --------------------------------------------------------------

#[derive(Debug, serde::Deserialize)]
pub struct AuthComplete {
    /// A pasted credential: either a provider API key or the long-lived OAuth
    /// token from `claude setup-token`. The engine tells them apart by prefix
    /// and routes each to its own environment variable — the caller does not
    /// have to know which it has, and cannot get it wrong by declaring it.
    #[serde(default)]
    pub api_key: Option<String>,
    /// Paste-code OAuth: what the browser showed the user.
    #[serde(default)]
    pub code: Option<String>,
    /// The handle from `auth/begin`. Optional, but supplying it stops a stale
    /// browser tab completing a login the user has already restarted.
    #[serde(default)]
    pub session: Option<Uuid>,
    /// A long-lived token from `claude setup-token`. Distinct from `api_key`
    /// only in that it ASSERTS durability: a short-lived credential submitted
    /// here is refused rather than quietly vaulted for five other agents to
    /// depend on.
    #[serde(default)]
    pub setup_token: Option<String>,
    /// Name of a vault node to also store the resulting credential in, so the
    /// other agents wired to that vault authenticate without their own login.
    /// The agent must have a read wire to it. Works with any of the three
    /// credential fields.
    #[serde(default)]
    pub save_to_vault: Option<String>,
    /// The vault key to store the credential under.
    ///
    /// Optional, and only ever a CONFIRMATION: the engine derives the right
    /// name from the credential itself, and a caller that disagrees is
    /// refused rather than obeyed. Letting a caller name the key would let
    /// them file an `ANTHROPIC_API_KEY` as `CLAUDE_CODE_OAUTH_TOKEN`, which
    /// every peer agent would then import under a variable the harness does
    /// not read (ADVERSARY 018).
    #[serde(default)]
    pub vault_key: Option<String>,
    /// Store a credential that EXPIRES into a vault other agents read.
    ///
    /// Refused without this, because the blast radius is every reader: when
    /// the session lapses they all stop at once, and a warning in a response
    /// body is a poor place to learn that. `claude setup-token` produces a
    /// credential that does not expire and needs no override.
    #[serde(default)]
    pub allow_shared_expiry: bool,
}

/// `POST /v1/agents/:id/auth/complete`
///
/// API-key mode today. The key is stored in the node's own credential
/// directory, which is what lets two agents in one sandbox be two accounts.
pub async fn auth_complete(
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<AuthComplete>,
) -> ApiResult<Json<serde_json::Value>> {
    let harness = agent_harness(&s, id)?;

    // `api-key-only`: refused at the door, before anything is stored or any
    // login child is fed a code, so a cloud project never holds an OAuth
    // credential even briefly. The spawn gate still backs this up.
    if api_key_only(&s) {
        if body.code.is_some() {
            s.logins.cancel(id).await;
            return Err(ApiError::policy_denied("an OAuth sign-in"));
        }
        if body.setup_token.is_some() {
            return Err(ApiError::policy_denied("a `claude setup-token` credential"));
        }
        let oauth_shaped = body.api_key.as_deref().is_some_and(|k| {
            crate::auth::classify_token(k, harness) == wheel_core::CredentialKind::OauthToken
        });
        if oauth_shaped {
            return Err(ApiError::policy_denied("an OAuth token"));
        }
    }

    // Paste-code OAuth: the code goes to the child that `auth/begin` left
    // waiting, and the CLI writes its own credentials into the staging dir.
    if let Some(code) = body.code {
        return finish_paste_code(
            &s,
            id,
            body.session,
            &code,
            body.save_to_vault.as_deref(),
            body.vault_key.as_deref(),
            body.allow_shared_expiry,
        )
        .await;
    }

    if let Some(token) = body.setup_token {
        return finish_setup_token(
            &s,
            id,
            harness,
            &token,
            body.save_to_vault.as_deref(),
            body.vault_key.as_deref(),
            body.allow_shared_expiry,
        )
        .await;
    }

    let Some(key) = body.api_key else {
        // Be explicit rather than silently succeeding with no credential: a
        // 200 here would leave the agent unauthenticated but looking fine.
        return Err(ApiError::invalid(
            "supply one of: setup_token (from `claude setup-token`), api_key (a provider key), \
             or code (paste-code OAuth)",
        ));
    };

    let config_dir = s.cfg.creds_dir().join(id.to_string());
    let kind = crate::auth::store_token(&config_dir, &key, harness)
        .map_err(|e| ApiError::invalid(e.to_string()))?;
    if harness == wheel_core::Harness::Codex {
        crate::auth::ensure_codex_file_store(&config_dir)
            .map_err(|e| ApiError::internal(e.to_string()))?;
    }

    // A provider key is shareable too -- one key across a board is a normal
    // thing to want -- so save_to_vault applies here as well.
    let vaulted = match body.save_to_vault.as_deref() {
        Some(vault) => {
            let found = crate::auth::StoredOauth::durable(key.trim());
            Some(save_credential_to_vault(
                &s,
                id,
                harness,
                vault,
                &found,
                body.vault_key.as_deref(),
                // A provider key carries no expiry, so the shared-vault
                // refusal cannot fire here; passed for the signature only.
                body.allow_shared_expiry,
            )?)
        }
        None => None,
    };

    resume_if_blocked(&s, id).await?;

    let mut out = serde_json::json!(wheel_core::AuthStatus {
        authenticated: true,
        mode: Some(kind),
        source: None,
        account: None,
        expires_at: None,
        refreshable: None,
        warning: None,
    });
    if let Some(v) = vaulted {
        out["vault"] = v;
    }
    Ok(Json(out))
}

/// `auth/complete {setup_token}` — the durable credential.
///
/// `claude setup-token` mints a long-lived token specifically so it can be
/// handed to other machines, which makes it the right thing to put in a vault
/// that a board of agents reads. This route exists as its own field rather
/// than as another `api_key` so it can REFUSE a short-lived credential: the
/// whole reason to use it is the promise that it will not expire underneath
/// five other agents, and accepting a session token here would break that
/// promise silently.
async fn finish_setup_token(
    s: &AppState,
    id: Uuid,
    harness: wheel_core::Harness,
    token: &str,
    save_to_vault: Option<&str>,
    vault_key: Option<&str>,
    allow_shared_expiry: bool,
) -> ApiResult<Json<serde_json::Value>> {
    let token = token.trim();
    if harness != wheel_core::Harness::Claude {
        return Err(ApiError::invalid(
            "setup_token is a claude credential; a codex node takes api_key",
        ));
    }
    let kind = crate::auth::classify_token(token, harness);
    if kind != wheel_core::CredentialKind::OauthToken {
        return Err(ApiError::invalid(
            "that is not a `claude setup-token` credential (expected one starting \
             `sk-ant-oat`); submit a provider key as api_key instead",
        ));
    }

    let config_dir = s.cfg.creds_dir().join(id.to_string());
    crate::auth::store_token(&config_dir, token, harness)
        .map_err(|e| ApiError::invalid(e.to_string()))?;

    let vaulted = match save_to_vault {
        Some(vault) => {
            // Durable because this route was TOLD so, not because nothing
            // mentioned an expiry: that is what makes the response carry no
            // warning.
            let found = crate::auth::StoredOauth::durable(token);
            Some(save_credential_to_vault(
                s,
                id,
                harness,
                vault,
                &found,
                vault_key,
                allow_shared_expiry,
            )?)
        }
        None => None,
    };

    resume_if_blocked(s, id).await?;

    let mut body = serde_json::json!(wheel_core::AuthStatus {
        authenticated: true,
        mode: Some(wheel_core::CredentialKind::OauthToken),
        source: None,
        account: None,
        // A `claude setup-token` credential carries no expiry; saying so is
        // different from saying we do not know.
        expires_at: None,
        refreshable: None,
        warning: None,
    });
    if let Some(v) = vaulted {
        body["vault"] = v;
    }
    Ok(Json(body))
}

/// An agent that stalled on `needs_auth` was already started by someone who
/// wanted it running, so saving a credential resumes it rather than leaving a
/// stuck queue for the operator to poke.
async fn resume_if_blocked(s: &AppState, id: Uuid) -> ApiResult<()> {
    let was_blocked = {
        let conn = s.db.lock().map_err(|_| ApiError::internal("db poisoned"))?;
        let st = board::agent_state(&conn, id).unwrap_or_default();
        let blocked = st.status == wheel_core::AgentStatus::NeedsAuth;
        if blocked {
            board::set_status(&conn, id, wheel_core::AgentStatus::Parked, None);
        }
        blocked
    };
    if was_blocked {
        let _ = s.supervisor.deliver(id).await;
    }
    s.events.publish(wheel_core::Event::BoardChanged {
        at: wheel_core::Timestamp::now(),
    });
    Ok(())
}

/// A fresh login in a vault un-sticks EVERY agent that reads it, not only the
/// one that signed in: they were all stopped by the same lapsed login.
async fn resume_readers_if_blocked(s: &AppState, vault: Uuid) -> ApiResult<()> {
    let blocked: Vec<Uuid> = {
        let conn = s.db.lock().map_err(|_| ApiError::internal("db poisoned"))?;
        crate::vault::agents_reading(&conn, vault)
            .unwrap_or_default()
            .into_iter()
            .filter(|a| {
                board::agent_state(&conn, *a).unwrap_or_default().status
                    == wheel_core::AgentStatus::NeedsAuth
            })
            .collect()
    };
    for agent in blocked {
        resume_if_blocked(s, agent).await?;
    }
    Ok(())
}

fn api_key_only(s: &AppState) -> bool {
    s.cfg.harness_auth == crate::config::HarnessAuthPolicy::ApiKeyOnly
}

/// Where a paste-code login writes: a directory the engine made for this one
/// login, not the node's own HOME.
///
/// An agent is untrusted code whose HOME is its config dir, so anything the
/// engine reads back from there after a login might be something the agent put
/// there instead. Nothing runs with this directory as its HOME but the login
/// child, so what is in it afterwards is what the login produced.
///
/// It is also load-bearing that this directory is EMPTY and THROWAWAY, not an
/// optimisation to be tidied away later into the node's own dir: the CLI
/// deletes `claudeAiOauth` from its store before writing the replacement
/// (`oV` runs ahead of the writer), so a login that dies in that window leaves
/// no credential at all. In a scratch directory that is harmless; in a
/// directory holding a credential somebody depends on it is a silent sign-out.
fn login_staging(s: &AppState, id: Uuid) -> std::path::PathBuf {
    s.cfg.data_dir.join("oauth-staging").join(id.to_string())
}

fn fresh_login_staging(s: &AppState, id: Uuid) -> ApiResult<std::path::PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let dir = login_staging(s, id);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| ApiError::internal(e.to_string()))?;
    for d in [dir.parent().unwrap_or(&dir), dir.as_path()] {
        std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| ApiError::internal(e.to_string()))?;
    }
    Ok(dir)
}

/// The harness an agent node is configured for, or a 404/400 that says why not.
fn agent_harness(s: &AppState, id: Uuid) -> ApiResult<wheel_core::Harness> {
    let conn = s.db.lock().map_err(|_| ApiError::internal("db poisoned"))?;
    let node = board::get(&conn, id)
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| ApiError::not_found(id.to_string()))?;
    Ok(node
        .config
        .as_agent()
        .ok_or_else(|| ApiError::invalid("not an agent node"))?
        .harness)
}

/// `POST /v1/agents/:id/auth/begin`
///
/// Starts a real sign-in against the user's own Anthropic account and returns
/// the URL they must visit. The CLI's redirect target is Anthropic-hosted, so
/// the container never needs a reachable localhost: the browser shows a code
/// and the user pastes it back through `auth/complete`.
///
/// The child stays alive between the two calls — that is the whole reason this
/// is stateful — and is killed if the user never returns.
pub async fn auth_begin(
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<wheel_core::AuthBegin>> {
    let harness = agent_harness(&s, id)?;
    // Before anything is spawned: on `api-key-only` no login session may
    // exist at all, not one that is refused when it finishes.
    if api_key_only(&s) {
        return Err(ApiError::policy_denied("an OAuth sign-in"));
    }
    if harness != wheel_core::Harness::Claude {
        // Codex signs in by device code, which is a poll rather than a submit
        // and is a different shape end to end. Saying so beats returning a
        // paste-code envelope that nothing on the other side can satisfy.
        return Err(ApiError::invalid(
            "codex uses device-code login, which is not implemented yet; \
             use auth/complete with an api_key for now",
        ));
    }

    s.logins.evict_expired().await;
    let staging = fresh_login_staging(&s, id)?;
    let program = s.supervisor.harness_program().to_string();

    let (session, url) = s
        .logins
        .begin(id, &program, &staging)
        .await
        .map_err(login_error)?;

    Ok(Json(wheel_core::AuthBegin {
        mode: wheel_core::AuthMode::PasteCode,
        url: Some(url),
        user_code: None,
        instructions: "Open the link, sign in to your Anthropic account, then paste the code \
                       it shows you back here."
            .to_string(),
        session,
    }))
}

async fn finish_paste_code(
    s: &AppState,
    id: Uuid,
    session: Option<Uuid>,
    code: &str,
    save_to_vault: Option<&str>,
    vault_key: Option<&str>,
    allow_shared_expiry: bool,
) -> ApiResult<Json<serde_json::Value>> {
    if code.trim().is_empty() {
        return Err(ApiError::invalid("the code is empty"));
    }
    s.logins
        .complete(id, session, code)
        .await
        .map_err(login_error)?;

    let harness = agent_harness(s, id)?;
    let staging = crate::auth::ScratchDir(login_staging(s, id));
    // Read strictly, from the staging dir only: the CLI's own store file, a
    // regular file, and exactly its claude.ai login. The CLI exiting happily
    // with nothing readable there is a 502, not an agent that looks signed in
    // and fails on its first turn.
    let captured = crate::auth::read_session(&staging.0).map_err(|e| {
        ApiError::new(
            StatusCode::BAD_GATEWAY,
            "harness_error",
            format!("the login reported success but left no usable credentials: {e}"),
        )
    })?;

    let mut refreshable = None;
    let vaulted = match save_to_vault {
        Some(vault) if captured.is_refreshable() => {
            let (vault_id, out) = save_session_to_vault(s, id, vault, &captured, vault_key)?;
            s.supervisor.session_saved(vault_id);
            resume_readers_if_blocked(s, vault_id).await?;
            refreshable = Some(true);
            Some(out)
        }
        Some(vault) => {
            let found = captured.as_stored().ok_or_else(|| {
                ApiError::new(
                    StatusCode::BAD_GATEWAY,
                    "harness_error",
                    "the login left no access token",
                )
            })?;
            Some(save_credential_to_vault(
                s,
                id,
                harness,
                vault,
                &found,
                vault_key,
                allow_shared_expiry,
            )?)
        }
        None => {
            let config_dir = s.cfg.creds_dir().join(id.to_string());
            crate::auth::install_store(&staging.0, &config_dir)
                .map_err(|e| ApiError::internal(e.to_string()))?;
            None
        }
    };
    drop(staging);

    resume_if_blocked(s, id).await?;

    let mut body = serde_json::json!(wheel_core::AuthStatus {
        authenticated: true,
        mode: Some(wheel_core::CredentialKind::OauthSession),
        source: None,
        account: None,
        expires_at: None,
        refreshable,
        warning: None,
    });
    if let Some(v) = vaulted {
        body["vault"] = v;
    }
    Ok(Json(body))
}

/// The vault a login may be saved to: it must exist, be a vault, and be one
/// this agent READS. The wire is the capability here as everywhere else.
fn target_vault(
    conn: &rusqlite::Connection,
    agent: Uuid,
    vault_name: &str,
) -> ApiResult<wheel_core::Node> {
    let vault = board::get_by_name(conn, vault_name)
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| ApiError::not_found(format!("no node named {vault_name:?}")))?;
    if vault.node_type() != wheel_core::NodeType::Vault {
        return Err(ApiError::invalid(format!("{vault_name} is not a vault")));
    }
    let me = board::get(conn, agent)
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| ApiError::not_found(agent.to_string()))?;
    if !me.has_wire(
        vault.id,
        wheel_core::WireType::Read,
        wheel_core::NodeType::Vault,
    ) {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "wire_denied",
            format!(
                "no wire from {} to {vault_name} (need: read) -- wire the agent to the vault first",
                me.name
            ),
        ));
    }
    Ok(vault)
}

fn peers_reading(conn: &rusqlite::Connection, vault: Uuid, except: Uuid) -> Vec<String> {
    crate::vault::agents_reading(conn, vault)
        .unwrap_or_default()
        .into_iter()
        .filter(|a| *a != except)
        .filter_map(|a| {
            board::get(conn, a)
                .ok()
                .flatten()
                .map(|n| n.name.to_string())
        })
        .collect()
}

/// Save a refreshable login — refresh token included — for the agents
/// reading `vault_name`.
///
/// The whole login goes in, so the engine can renew it before it lapses
/// (docs/proposals/harness-oauth-refresh.md). No `shared_expiry` refusal:
/// ADVERSARY 021's blast radius was a shared expiry, and this one is renewed
/// rather than allowed to lapse. It supersedes a bare `CLAUDE_CODE_OAUTH_TOKEN`
/// in the same vault, which would otherwise be a second value for the same
/// slot.
fn save_session_to_vault(
    s: &AppState,
    agent: Uuid,
    vault_name: &str,
    session: &crate::auth::OauthSession,
    requested_key: Option<&str>,
) -> ApiResult<(Uuid, serde_json::Value)> {
    const KEY: &str = wheel_core::CLAUDE_OAUTH_SESSION;
    // The same ceiling a renewal applies, reported the same way: this sign-in
    // is where the operator finds out what the server actually issues.
    let mut clamped = session.clone();
    let now = (time::OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000) as i64;
    let server_said = clamped.clamp_expiry(now, crate::auth::MAX_RECORDED_LIFETIME_MS);
    let session = &clamped;
    let conn = s.db.lock().map_err(|_| ApiError::internal("db poisoned"))?;
    let vault = target_vault(&conn, agent, vault_name)?;

    // A confirmation, never an instruction (018). Either name the caller may
    // know it by is accepted; anything else is a caller bug named back to it.
    if let Some(requested) = requested_key {
        let agrees = requested.eq_ignore_ascii_case(KEY)
            || requested.eq_ignore_ascii_case("CLAUDE_CODE_OAUTH_TOKEN");
        if !agrees {
            return Err(ApiError::invalid(format!(
                "this is a refreshable claude.ai login and is stored as {KEY} (agents receive it \
                 as CLAUDE_CODE_OAUTH_TOKEN), not {requested:?}"
            )));
        }
    }

    let expires_at = session
        .expires_at()
        .and_then(crate::vault::millis_to_timestamp);
    let declared_overlap = crate::api::vault_routes::store_in_vault_until(
        s,
        &conn,
        vault.id,
        KEY,
        &session.to_vault_value(),
        expires_at,
    )?;
    let superseded = crate::vault::list_keys(&conn, vault.id)
        .map_err(|e| ApiError::internal(e.to_string()))?
        .iter()
        .any(|k| k == "CLAUDE_CODE_OAUTH_TOKEN");
    if superseded {
        crate::api::vault_routes::remove_from_vault(&conn, vault.id, "CLAUDE_CODE_OAUTH_TOKEN")?;
    }

    drop(conn);
    s.supervisor
        .record_login_facts(vault.id, session, server_said, "sign-in");
    let conn = s.db.lock().map_err(|_| ApiError::internal("db poisoned"))?;
    let peers = peers_reading(&conn, vault.id, agent);
    let mut out = serde_json::json!({
        "name": vault_name,
        "key": KEY,
        "exports_as": "CLAUDE_CODE_OAUTH_TOKEN",
        "stored": true,
        "refreshable": true,
    });
    if let Some(exp) = expires_at {
        out["expires_at"] = serde_json::json!(exp);
    }
    if !peers.is_empty() {
        out["shared_with"] = serde_json::json!(peers);
    }
    if let Some(w) = declared_overlap {
        out["warning"] = serde_json::json!(w);
    }
    Ok((vault.id, out))
}

/// Copy the credential this login just produced into a vault, so the other
/// agents wired to that vault do not each need their own browser round-trip.
///
/// Reports the expiry rather than hiding it. A subscription login stores a
/// SESSION token that the CLI refreshes in place; copying it into a vault
/// gives five other agents a credential that works now and stops working
/// later, with nothing to explain why. `claude setup-token` is the durable
/// answer, and the response says so when what we found is not that.
fn save_credential_to_vault(
    s: &AppState,
    agent: Uuid,
    harness: wheel_core::Harness,
    vault_name: &str,
    found: &crate::auth::StoredOauth,
    requested_key: Option<&str>,
    allow_shared_expiry: bool,
) -> ApiResult<serde_json::Value> {
    let conn = s.db.lock().map_err(|_| ApiError::internal("db poisoned"))?;
    // The wire is the capability here as everywhere else: an agent may only
    // put its credential in a vault it can actually read, or it would be
    // writing into a keyspace it has no relationship with.
    let vault = target_vault(&conn, agent, vault_name)?;

    // ADVERSARY 021: a session credential in a SHARED vault strands every
    // reader at once when it lapses -- `lapsed_credential` gates each agent on
    // the vault's expiry, so N readers become N stopped agents for one
    // expiry. The setup_token path already refuses a non-durable credential
    // for exactly this reason; a warning in a response body is not the same
    // protection, and the person who reads it is not the person stranded.
    //
    // Refused by DEFAULT, with an explicit override rather than a hard no:
    // an operator with no CLI cannot run `claude setup-token`, and for them
    // paste-code + save_to_vault is the only way to authenticate a board. The
    // refusal makes it a decision instead of a surprise.
    let peers = peers_reading(&conn, vault.id, agent);
    if !peers.is_empty() && !found.is_long_lived() && !allow_shared_expiry {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "shared_expiry",
            format!(
                "this credential expires, and {} also read{} {vault_name}: when it lapses they all \
                 stop at once. Use a `claude setup-token` credential, which does not expire, or \
                 resend with allow_shared_expiry to accept that.",
                peers.join(", "),
                if peers.len() == 1 { "s" } else { "" }
            ),
        ));
    }

    // The env var the credential ACTUALLY is, not a fixed one (ADVERSARY,
    // finding 018). Vaulting an `ANTHROPIC_API_KEY` under
    // `CLAUDE_CODE_OAUTH_TOKEN` would export it to every peer agent under a
    // name the harness does not read, and they would all fail to authenticate
    // with a credential that is sitting right there and perfectly valid.
    // These are the same two functions that route a pasted credential into a
    // child's environment, so the vault and the spawn cannot disagree.
    let kind = crate::auth::classify_token(&found.token, harness);
    let key = crate::auth::token_env(kind, harness);

    // An explicit key is a confirmation, never an instruction. Disagreeing
    // with the credential is the exact mistake 018 described, so it is a 400
    // that names both -- silently correcting it would hide a caller bug, and
    // obeying it would recreate the leak.
    if let Some(requested) = requested_key {
        if !requested.eq_ignore_ascii_case(key) {
            return Err(ApiError::invalid(format!(
                "this credential is a {} and must be stored as {key}, not {requested:?}",
                kind.as_str()
            )));
        }
    }

    // RFC3339, not the store's raw milliseconds: §2 says every time on this
    // API is RFC3339 UTC, and the UI renders this one directly.
    let expires_at = found.expires_at.and_then(crate::vault::millis_to_timestamp);
    let declared_overlap = crate::api::vault_routes::store_in_vault_until(
        s,
        &conn,
        vault.id,
        key,
        &found.token,
        expires_at,
    )?;

    let mut out = serde_json::json!({ "name": vault_name, "key": key, "stored": true });
    if let Some(exp) = expires_at {
        out["expires_at"] = serde_json::json!(exp);
    }
    if !peers.is_empty() {
        out["shared_with"] = serde_json::json!(peers);
    }
    // Two independent reasons a login can carry a warning; join both rather
    // than letting one clobber the other silently.
    let mut warnings = Vec::new();
    if !found.is_long_lived() {
        warnings.push(
            "this is a session credential and will expire; for a durable one, \
             run `claude setup-token` and submit that token as api_key instead"
                .to_string(),
        );
    }
    if let Some(w) = declared_overlap {
        warnings.push(w);
    }
    if !warnings.is_empty() {
        out["warning"] = serde_json::json!(warnings.join(" / "));
    }
    Ok(out)
}

fn login_error(e: crate::oauth::LoginError) -> ApiError {
    use crate::oauth::LoginError as L;
    match e {
        // Gone, not malformed: the client should start again, not retry.
        L::NoSession | L::Expired => ApiError::new(StatusCode::CONFLICT, "expired", e.to_string()),
        L::Rejected(_) => ApiError::invalid(e.to_string()),
        L::Timeout => ApiError::new(StatusCode::GATEWAY_TIMEOUT, "timeout", e.to_string()),
        // 400, not 504: a 5xx here is relayed to the operator as a gateway
        // timeout, which reads as "the service is broken" when the actual
        // situation is that their code produced no verdict and they should
        // start again. The message is the useful part and it is theirs to act
        // on.
        L::NoResponse => ApiError::invalid(e.to_string()),
        L::Spawn(m) => ApiError::new(StatusCode::BAD_GATEWAY, "harness_error", m),
    }
}

/// `GET /v1/agents/:id/auth`
///
/// Reports whether credentials are STORED, which is not the same as whether
/// they work — only the harness's own probe can say that, and claiming
/// otherwise would tell an operator they are authenticated right up until the
/// first request fails.
pub async fn auth_status(
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    let harness = agent_harness(&s, id)?;

    // A wired vault wins over a pasted credential: it is the thing the
    // operator can see and change on the board, and it is how one project runs
    // several accounts of the same provider. Reporting the pasted one while
    // the vault supplies the value the child actually runs with would be a lie
    // about which account is live.
    let (from_vault, session_vault) = {
        let conn = s.db.lock().map_err(|_| ApiError::internal("db poisoned"))?;
        (
            crate::vault::credential_detail(&conn, id, harness).unwrap_or(None),
            crate::vault::session_vault_for(&conn, id).unwrap_or(None),
        )
    };
    if let Some((source, key, expires_at)) = from_vault {
        // A refreshable login's `expires_at` is when the CURRENT token ends;
        // the engine renews it before then. The warning is what the operator
        // must act on, and it arrives before any agent is refused.
        let (refreshable, warning) = match session_vault {
            Some(vault) if key == wheel_core::CLAUDE_OAUTH_SESSION => {
                (Some(true), s.supervisor.refresh_warning(vault))
            }
            _ => (None, None),
        };
        return Ok(Json(serde_json::json!(wheel_core::AuthStatus {
            authenticated: true,
            mode: Some(wheel_core::CredentialKind::Env),
            source: Some(source),
            account: None,
            // Absent means durable OR unknown. The UI shows "re-login by ..."
            // only when there is a real time to show, rather than inventing a
            // deadline for a credential nobody said anything about.
            expires_at,
            refreshable,
            warning,
        })));
    }

    let config_dir = s.cfg.creds_dir().join(id.to_string());
    let authenticated = crate::auth::has_stored_credentials(&config_dir, harness);
    // A stored token names its own kind. Otherwise credentials, if any, are
    // the harness's own login on disk. Nothing stored reports `null` rather
    // than a mode it does not have.
    let mode = crate::auth::stored_token_kind(&config_dir, harness).or({
        if authenticated {
            Some(wheel_core::CredentialKind::OauthSession)
        } else {
            None
        }
    });

    // For a login on disk, the harness's own store is the only thing that
    // knows when it lapses -- and it is the same store the child reads, so
    // this is the truth rather than a copy of it.
    let expires_at = if mode == Some(wheel_core::CredentialKind::OauthSession) {
        // No freshness floor here: this only REPORTS an expiry for display. An
        // agent that lies about its own credential's expiry misleads the UI
        // about itself and nothing else -- and at worst refuses to start,
        // which harms only it.
        crate::auth::oauth_token_from_store(&config_dir, None)
            .ok()
            .and_then(|t| t.expires_at)
            .and_then(crate::vault::millis_to_timestamp)
    } else {
        None
    };

    Ok(Json(serde_json::json!(wheel_core::AuthStatus {
        authenticated,
        mode,
        source: None,
        account: None,
        expires_at,
        refreshable: None,
        warning: None,
    })))
}

/// `DELETE /v1/agents/:id/auth` — forget stored credentials.
pub async fn auth_clear(State(s): State<AppState>, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    let config_dir = s.cfg.creds_dir().join(id.to_string());
    crate::auth::clear_token(&config_dir).map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(StatusCode::NO_CONTENT)
}

/// QA's coverage gap on 028 face 5: `save_credential_to_vault`'s warning-join
/// (the part this feature added here) had no direct test at all. The two
/// warnings come from independent conditions -- a session credential, and a
/// declared-overlap with another wired vault -- and the code joins them with
/// `" / "` rather than letting one clobber the other. Nothing else in this
/// crate exercises both branches firing on the same call.
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use wheel_core::{AgentConfig, Node, NodeConfig, Position, VaultConfig, WireType};

    fn mk(conn: &rusqlite::Connection, name: &str, config: NodeConfig) -> Uuid {
        let n = Node::new(
            Uuid::new_v4(),
            name.parse().unwrap(),
            Position::default(),
            config,
        );
        board::create(conn, &n).unwrap();
        n.id
    }

    #[test]
    fn both_warnings_join_with_a_slash_and_neither_clobbers_the_other() {
        let state = crate::api::test_state();
        let agent = {
            let conn = state.db.lock().unwrap();
            let agent = mk(&conn, "agent", NodeConfig::Agent(AgentConfig::default()));
            // The vault the credential is actually written to.
            let creds = mk(
                &conn,
                "creds",
                NodeConfig::Vault(VaultConfig { keys: vec![] }),
            );
            // A second vault this same agent reads that DECLARES the same
            // key -- nothing stored there, so this is a warning, not a
            // block (028 face 5's own rule), and it must surface alongside
            // the session-credential warning rather than instead of it.
            let other = mk(
                &conn,
                "other-creds",
                NodeConfig::Vault(VaultConfig {
                    keys: vec!["CLAUDE_CODE_OAUTH_TOKEN".into()],
                }),
            );
            board::add_wire(&conn, agent, creds, WireType::Read, None).unwrap();
            board::add_wire(&conn, agent, other, WireType::Read, None).unwrap();
            agent
        };

        // OAuth-shaped (so it classifies as CLAUDE_CODE_OAUTH_TOKEN, the same
        // key `other` declares) but carrying an expiry, so `is_long_lived` is
        // false and the session-credential warning fires too. No other agent
        // reads `creds`, so the shared-expiry refusal (ADVERSARY 021) never
        // triggers and both warnings reach the response instead of an error.
        let found = crate::auth::StoredOauth {
            token: "sk-ant-oat01-session".into(),
            expires_at: Some(4_102_444_800_000),
            durable: false,
        };

        let resp = save_credential_to_vault(
            &state,
            agent,
            wheel_core::Harness::Claude,
            "creds",
            &found,
            None,
            false,
        )
        .expect("neither warning may block the write");

        let warning = resp["warning"]
            .as_str()
            .expect("both warnings must be present in the response");
        assert!(
            warning.contains("session credential"),
            "the session-credential warning must survive the join: {warning}"
        );
        assert!(
            warning.contains("also declares"),
            "the declared-overlap warning must survive the join: {warning}"
        );
        assert!(
            warning.contains(" / "),
            "the two warnings must be joined with \" / \", not concatenated or replaced: {warning}"
        );
    }

    // --- the headless sign-in, end to end (docs/proposals/harness-oauth-refresh.md) -----

    const FAKE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../qa/harness/fake-claude");

    /// An engine whose `claude` is the QA fake, with a fake token store behind
    /// it, so `auth/begin` + `auth/complete` run the REAL two-call flow — a
    /// live child between them — without a browser or an account.
    struct Signin {
        state: AppState,
        dir: std::path::PathBuf,
        agent: Uuid,
        vault: Uuid,
    }

    impl Signin {
        fn new(name: &str, policy: crate::config::HarnessAuthPolicy) -> Self {
            use std::os::unix::fs::PermissionsExt;
            let dir = std::env::temp_dir().join(format!(
                "wheel-signin-{name}-{}-{}",
                std::process::id(),
                Uuid::new_v4()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            let store = dir.join("token-store.json");
            std::fs::write(
                &store,
                serde_json::json!({
                    "access": {}, "refresh": {},
                    "lifetime_ms": 8 * 3_600_000i64, "rotate": true,
                    "refreshes": 0, "counter": 0,
                })
                .to_string(),
            )
            .unwrap();
            let fake_cfg = dir.join("fake.json");
            std::fs::write(
                &fake_cfg,
                serde_json::json!({
                    "token_store": store.display().to_string(),
                    "login_account": "acct-A",
                    "login_code": "fake-auth-code",
                })
                .to_string(),
            )
            .unwrap();
            let program = dir.join("claude.sh");
            std::fs::write(
                &program,
                format!(
                    "#!/bin/sh\nexport WHEEL_FAKE_CONFIG='{}'\nexec python3 '{FAKE}' \"$@\"\n",
                    fake_cfg.display()
                ),
            )
            .unwrap();
            std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();

            let program = program.display().to_string();
            let state = crate::api::test_state_with(
                policy,
                Some(Arc::new(crate::harness::claude::ProgramDriver(
                    program.clone(),
                ))),
                |sup| sup.broker.program = Some(program),
            );
            let mut cfg = (*state.cfg).clone();
            cfg.data_dir = dir.clone();
            let state = AppState {
                cfg: Arc::new(cfg),
                ..state
            };

            let (agent, vault) = {
                let conn = state.db.lock().unwrap();
                let agent = mk(&conn, "worker", NodeConfig::Agent(AgentConfig::default()));
                let vault = mk(
                    &conn,
                    "anthropic",
                    NodeConfig::Vault(VaultConfig { keys: vec![] }),
                );
                board::add_wire(&conn, agent, vault, WireType::Read, None).unwrap();
                (agent, vault)
            };
            Self {
                state,
                dir,
                agent,
                vault,
            }
        }

        async fn begin(&self) -> ApiResult<wheel_core::AuthBegin> {
            auth_begin(State(self.state.clone()), Path(self.agent))
                .await
                .map(|j| j.0)
        }

        async fn complete(&self, body: serde_json::Value) -> ApiResult<serde_json::Value> {
            auth_complete(
                State(self.state.clone()),
                Path(self.agent),
                Json(serde_json::from_value(body).unwrap()),
            )
            .await
            .map(|j| j.0)
        }

        fn session(&self) -> Option<crate::auth::OauthSession> {
            let conn = self.state.db.lock().unwrap();
            crate::vault::get_session(
                &conn,
                self.state.supervisor.vault_key().unwrap(),
                self.vault,
            )
            .unwrap()
        }

        fn node_dir(&self) -> std::path::PathBuf {
            self.state.cfg.creds_dir().join(self.agent.to_string())
        }
    }

    impl Drop for Signin {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// The VPS case: no browser, an SSH tunnel, and a login that has to be
    /// completed by pasting a code back. What the vault ends up holding is the
    /// WHOLE login — refresh token included — so the engine can renew it.
    #[tokio::test]
    async fn a_headless_sign_in_saves_a_renewable_login_to_the_vault() {
        let rig = Signin::new("headless", crate::config::HarnessAuthPolicy::OauthToken);

        let begun = rig.begin().await.expect("a paste-code login starts");
        assert_eq!(begun.mode, wheel_core::AuthMode::PasteCode);
        assert!(
            begun
                .url
                .as_deref()
                .unwrap_or_default()
                .starts_with("https://"),
            "the operator needs a URL to open on their own machine: {begun:?}"
        );

        let body = rig
            .complete(serde_json::json!({
                "code": "fake-auth-code",
                "session": begun.session,
                "save_to_vault": "anthropic",
            }))
            .await
            .expect("the pasted code completes the login");

        assert_eq!(body["vault"]["key"], wheel_core::CLAUDE_OAUTH_SESSION);
        assert_eq!(body["vault"]["refreshable"], true);
        assert_eq!(body["vault"]["exports_as"], "CLAUDE_CODE_OAUTH_TOKEN");
        assert!(
            body["vault"].get("warning").is_none(),
            "a renewable login needs no expiry warning: {}",
            body["vault"]
        );

        let session = rig.session().expect("the vault holds the login");
        assert!(
            session.is_refreshable(),
            "refresh token and scopes included"
        );
        assert!(session.refresh_token().unwrap().starts_with("sk-ant-ort"));
        assert_eq!(session.account().account_uuid.as_deref(), Some("acct-A"));

        // The login never lands in the agent's own HOME, and the staging dir
        // it was captured in is gone.
        assert!(!rig.node_dir().join(".credentials.json").exists());
        assert!(!login_staging(&rig.state, rig.agent).exists());

        // ...and what the agent will actually run with is the access token.
        let env = {
            let conn = rig.state.db.lock().unwrap();
            crate::vault::env_for_agent(&conn, rig.state.supervisor.vault_key().unwrap(), rig.agent)
                .unwrap()
        };
        assert_eq!(env.len(), 1);
        assert_eq!(env[0].0, "CLAUDE_CODE_OAUTH_TOKEN");
        assert_eq!(Some(env[0].1.as_str()), session.access_token());
    }

    /// TH1 at the capture. An agent is untrusted code whose HOME is its own
    /// config dir: it plants another account's credentials there, in both
    /// layouts, while the operator is signing in. The login is read from the
    /// engine's own staging dir, so the planted one is never what is saved.
    ///
    /// Mutation-checked: read the session from the node's config dir instead
    /// and the planted account is what every peer agent gets.
    #[tokio::test]
    async fn a_credential_planted_in_the_agents_home_is_not_what_a_sign_in_saves() {
        let rig = Signin::new("planted", crate::config::HarnessAuthPolicy::OauthToken);
        let begun = rig.begin().await.unwrap();

        let planted = serde_json::json!({"claudeAiOauth": {
            "accessToken": "sk-ant-oat01-PLANTED",
            "refreshToken": "sk-ant-ort01-PLANTED",
            "expiresAt": 4_102_444_800_000i64,
            "scopes": ["user:inference"],
        }})
        .to_string();
        let home = rig.node_dir();
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        for p in [
            home.join(".credentials.json"),
            home.join(".claude/.credentials.json"),
        ] {
            std::fs::write(p, &planted).unwrap();
        }

        rig.complete(serde_json::json!({
            "code": "fake-auth-code",
            "session": begun.session,
            "save_to_vault": "anthropic",
        }))
        .await
        .unwrap();

        let session = rig.session().unwrap();
        assert!(
            !session.access_token().unwrap().contains("PLANTED"),
            "an agent-planted credential was promoted to the vault: {:?}",
            session.access_token()
        );
        assert!(!session.refresh_token().unwrap().contains("PLANTED"));
    }

    /// A sign-in with no vault keeps the login for that one agent, in its own
    /// config dir — option (c), the per-agent login, still works.
    #[tokio::test]
    async fn a_sign_in_without_a_vault_installs_the_login_in_the_agents_own_dir() {
        let rig = Signin::new("pernode", crate::config::HarnessAuthPolicy::OauthToken);
        let begun = rig.begin().await.unwrap();
        rig.complete(serde_json::json!({ "code": "fake-auth-code", "session": begun.session }))
            .await
            .unwrap();

        let installed = crate::auth::read_session(&rig.node_dir()).expect("its own store");
        assert!(installed.is_refreshable());
        assert!(rig.session().is_none(), "nothing was put in the vault");
        assert!(!login_staging(&rig.state, rig.agent).exists());
    }

    /// The orchestrator's ask, and wheel-harness-auth.md's enforcement points
    /// 1 and 2: on an api-key-only deployment no OAuth credential may be
    /// created, and no login child may even be started.
    ///
    /// Mutation-checked: remove either policy check and this fails — the
    /// begin case by returning a URL, the complete cases by storing.
    #[tokio::test]
    async fn api_key_only_refuses_every_oauth_way_in_before_a_login_exists() {
        let rig = Signin::new("policy", crate::config::HarnessAuthPolicy::ApiKeyOnly);

        let refused = rig.begin().await.expect_err("no OAuth sign-in here");
        assert_eq!(refused.0, StatusCode::FORBIDDEN);
        assert_eq!(refused.1, "policy_denied");
        assert!(
            !login_staging(&rig.state, rig.agent).exists(),
            "no login may be started at all"
        );

        for body in [
            serde_json::json!({"code": "fake-auth-code"}),
            serde_json::json!({"setup_token": "sk-ant-oat01-durable"}),
            serde_json::json!({"api_key": "sk-ant-oat01-smuggled"}),
        ] {
            let err = rig
                .complete(body.clone())
                .await
                .expect_err("an OAuth credential is refused");
            assert_eq!(err.0, StatusCode::FORBIDDEN, "{body}");
            assert_eq!(err.1, "policy_denied", "{body}");
        }

        // An API key is what this deployment is for, and it still works.
        rig.complete(serde_json::json!({"api_key": "sk-ant-api03-real"}))
            .await
            .expect("an api key is the point of api-key-only");
    }

    /// A renewable login and a bare token are the same variable in the child,
    /// so one vault cannot hold both: the login supersedes the token it
    /// replaces rather than leaving two values for one slot.
    #[tokio::test]
    async fn a_saved_login_supersedes_a_bare_token_in_the_same_vault() {
        let rig = Signin::new("supersede", crate::config::HarnessAuthPolicy::OauthToken);
        {
            let conn = rig.state.db.lock().unwrap();
            crate::api::vault_routes::store_in_vault(
                &rig.state,
                &conn,
                rig.vault,
                "CLAUDE_CODE_OAUTH_TOKEN",
                "sk-ant-oat01-older",
            )
            .unwrap();
        }

        let begun = rig.begin().await.unwrap();
        rig.complete(serde_json::json!({
            "code": "fake-auth-code",
            "session": begun.session,
            "save_to_vault": "anthropic",
        }))
        .await
        .unwrap();

        let keys = {
            let conn = rig.state.db.lock().unwrap();
            crate::vault::list_keys(&conn, rig.vault).unwrap()
        };
        assert_eq!(
            keys,
            vec![wheel_core::CLAUDE_OAUTH_SESSION.to_string()],
            "the bare token it replaces must be gone, not left beside it"
        );
    }
}
