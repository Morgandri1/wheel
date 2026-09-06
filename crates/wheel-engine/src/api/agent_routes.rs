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
    /// Name of a vault node to also store the resulting credential in, so the
    /// other agents wired to that vault authenticate without their own login.
    /// The agent must have a read wire to it.
    #[serde(default)]
    pub save_to_vault: Option<String>,
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

    // Paste-code OAuth: the code goes to the child that `auth/begin` left
    // waiting, and the CLI writes its own credentials into the node's dir.
    if let Some(code) = body.code {
        return finish_paste_code(&s, id, body.session, &code, body.save_to_vault.as_deref()).await;
    }

    let Some(key) = body.api_key else {
        // Be explicit rather than silently succeeding with no credential: a
        // 200 here would leave the agent unauthenticated but looking fine.
        return Err(ApiError::invalid(
            "supply either api_key (a provider key or a `claude setup-token`) or code (paste-code OAuth)",
        ));
    };

    let config_dir = s.cfg.creds_dir().join(id.to_string());
    let kind = crate::auth::store_token(&config_dir, &key, harness)
        .map_err(|e| ApiError::invalid(e.to_string()))?;
    if harness == wheel_core::Harness::Codex {
        crate::auth::ensure_codex_file_store(&config_dir)
            .map_err(|e| ApiError::internal(e.to_string()))?;
    }

    // An agent that stalled on needs_auth was already started by someone who
    // wanted it running, so saving a credential resumes it rather than leaving
    // a stuck queue for the operator to poke. `parked` is the honest status
    // for that: logically on, no process — and `deliver` below spawns one only
    // if a message is actually waiting, so an agent with an empty queue costs
    // nothing while it sits authenticated.
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

    Ok(Json(serde_json::json!(wheel_core::AuthStatus {
        authenticated: true,
        mode: Some(kind),
        source: None,
        account: None,
    })))
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
    let config_dir = s.cfg.creds_dir().join(id.to_string());
    let program = s.supervisor.harness_program().to_string();

    let (session, url) = s
        .logins
        .begin(id, &program, &config_dir)
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
) -> ApiResult<Json<serde_json::Value>> {
    if code.trim().is_empty() {
        return Err(ApiError::invalid("the code is empty"));
    }
    s.logins
        .complete(id, session, code)
        .await
        .map_err(login_error)?;

    let harness = agent_harness(s, id)?;
    let config_dir = s.cfg.creds_dir().join(id.to_string());
    if !crate::auth::has_stored_credentials(&config_dir, harness) {
        // The CLI exited happily but wrote nothing we can find. Reporting
        // success here would leave an agent that looks signed in and fails on
        // its first turn.
        return Err(ApiError::new(
            StatusCode::BAD_GATEWAY,
            "harness_error",
            "the login reported success but left no credentials",
        ));
    }

    let vaulted = match save_to_vault {
        Some(vault) => Some(save_credential_to_vault(s, id, &config_dir, vault)?),
        None => None,
    };

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

    let mut body = serde_json::json!(wheel_core::AuthStatus {
        authenticated: true,
        mode: Some(wheel_core::CredentialKind::OauthSession),
        source: None,
        account: None,
    });
    if let Some(v) = vaulted {
        body["vault"] = v;
    }
    Ok(Json(body))
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
    config_dir: &std::path::Path,
    vault_name: &str,
) -> ApiResult<serde_json::Value> {
    let found = crate::auth::oauth_token_from_store(config_dir)
        .map_err(|e| ApiError::new(StatusCode::BAD_GATEWAY, "harness_error", e.to_string()))?;

    let conn = s.db.lock().map_err(|_| ApiError::internal("db poisoned"))?;
    let vault = board::get_by_name(&conn, vault_name)
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| ApiError::not_found(format!("no node named {vault_name:?}")))?;
    if vault.node_type() != wheel_core::NodeType::Vault {
        return Err(ApiError::invalid(format!("{vault_name} is not a vault")));
    }

    // The wire is the capability here as everywhere else: an agent may only
    // put its credential in a vault it can actually read, or it would be
    // writing into a keyspace it has no relationship with.
    let me = board::get(&conn, agent)
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

    const KEY: &str = "CLAUDE_CODE_OAUTH_TOKEN";
    crate::api::vault_routes::store_in_vault(s, &conn, vault.id, KEY, &found.token)?;

    let mut out = serde_json::json!({ "name": vault_name, "key": KEY, "stored": true });
    if let Some(exp) = found.expires_at {
        out["expires_at"] = serde_json::json!(exp);
    }
    if !found.is_long_lived() {
        out["warning"] = serde_json::json!(
            "this is a session credential and will expire; for a durable one, run              `claude setup-token` and submit that token as api_key instead"
        );
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
    // A vault wins over a pasted credential: it is the thing the operator can
    // see and change on the board, and it is how a project runs several
    // accounts. Reporting the pasted one while the vault supplies the value
    // the child actually runs with would be a lie about which account is live.
    let vault_source = {
        let conn = s.db.lock().map_err(|_| ApiError::internal("db poisoned"))?;
        crate::vault::credential_source(&conn, id, harness).unwrap_or(None)
    };
    if let Some(source) = vault_source {
        return Ok(Json(serde_json::json!(wheel_core::AuthStatus {
            authenticated: true,
            mode: Some(wheel_core::CredentialKind::Env),
            source: Some(source),
            account: None,
        })));
    }

    // A wired vault wins: it is the thing the operator can see and change on
    // the board, and it is how one project runs several accounts of the same
    // provider. A pasted credential is the fallback, not the other way round.
    let vault_source = {
        let conn = s.db.lock().map_err(|_| ApiError::internal("db poisoned"))?;
        crate::vault::credential_source(&conn, id, harness).unwrap_or(None)
    };
    if let Some(source) = vault_source {
        return Ok(Json(serde_json::json!(wheel_core::AuthStatus {
            authenticated: true,
            mode: Some(wheel_core::CredentialKind::Env),
            source: Some(source),
            account: None,
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

    Ok(Json(serde_json::json!(wheel_core::AuthStatus {
        authenticated,
        mode,
        source: None,
        account: None,
    })))
}

/// `DELETE /v1/agents/:id/auth` — forget stored credentials.
pub async fn auth_clear(State(s): State<AppState>, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    let config_dir = s.cfg.creds_dir().join(id.to_string());
    crate::auth::clear_token(&config_dir).map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(StatusCode::NO_CONTENT)
}
