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
    let _ = s.supervisor.pump_queue(id).await;
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
    let _ = s.supervisor.pump_queue(id).await;
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
    let _ = s.supervisor.pump_queue(id).await;

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
    if let Some(stream) = q.stream.as_deref() {
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
            rusqlite::params![id.to_string(), since, q.stream, limit],
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
    /// API-key mode. OAuth modes carry a `code` instead and land in M2.
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub code: Option<String>,
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
    let harness = {
        let conn = s.db.lock().map_err(|_| ApiError::internal("db poisoned"))?;
        let node = board::get(&conn, id)
            .map_err(|e| ApiError::internal(e.to_string()))?
            .ok_or_else(|| ApiError::not_found(id.to_string()))?;
        node.config
            .as_agent()
            .ok_or_else(|| ApiError::invalid("not an agent node"))?
            .harness
    };

    let Some(key) = body.api_key else {
        // Be explicit rather than silently succeeding with no credential: a
        // 200 here would leave the agent unauthenticated but looking fine.
        return Err(ApiError::invalid(
            "api_key is required; paste-code and device-code OAuth land in M2",
        ));
    };

    let config_dir = s.cfg.creds_dir().join(id.to_string());
    crate::auth::store_api_key(&config_dir, &key).map_err(|e| ApiError::invalid(e.to_string()))?;
    if harness == wheel_core::Harness::Codex {
        crate::auth::ensure_codex_file_store(&config_dir)
            .map_err(|e| ApiError::internal(e.to_string()))?;
    }

    // A queued message that stalled on needs_auth is an ENVIRONMENTAL failure,
    // not a poison message, so it stays queued and the agent is moved back to
    // stopped — the next start drains it (§ message delivery contract).
    {
        let conn = s.db.lock().map_err(|_| ApiError::internal("db poisoned"))?;
        let st = board::agent_state(&conn, id).unwrap_or_default();
        if st.status == wheel_core::AgentStatus::NeedsAuth {
            board::set_status(&conn, id, wheel_core::AgentStatus::Stopped, None);
        }
    }
    s.events.publish(wheel_core::Event::BoardChanged {
        at: wheel_core::Timestamp::now(),
    });

    Ok(Json(serde_json::json!({
        "authenticated": true,
        "mode": "api_key",
    })))
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
    let harness = {
        let conn = s.db.lock().map_err(|_| ApiError::internal("db poisoned"))?;
        let node = board::get(&conn, id)
            .map_err(|e| ApiError::internal(e.to_string()))?
            .ok_or_else(|| ApiError::not_found(id.to_string()))?;
        node.config
            .as_agent()
            .ok_or_else(|| ApiError::invalid("not an agent node"))?
            .harness
    };
    let config_dir = s.cfg.creds_dir().join(id.to_string());
    let has_key = crate::auth::read_api_key(&config_dir).is_some();

    Ok(Json(serde_json::json!({
        "authenticated": crate::auth::has_stored_credentials(&config_dir, harness),
        "mode": if has_key { "api_key" } else { "oauth" },
    })))
}

/// `DELETE /v1/agents/:id/auth` — forget stored credentials.
pub async fn auth_clear(State(s): State<AppState>, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    let config_dir = s.cfg.creds_dir().join(id.to_string());
    crate::auth::clear_api_key(&config_dir).map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(StatusCode::NO_CONTENT)
}
