//! `/v1/cli/*` — what the `wheel` binary calls.
//!
//! A different auth realm from the rest of `/v1`: these routes accept a
//! per-node token and NEVER the engine secret. Every one of them resolves its
//! authority through [`crate::caps::Caller`], so there is no path to a data
//! node that skips the wire check.

use axum::{
    extract::State,
    http::{header, HeaderMap, StatusCode},
    Json,
};
use serde::{Deserialize, Serialize};
use wheel_core::{
    Event, LogStream, Message, MessageReceipt, MessageSender, NodeType, Timestamp, WireDenial,
    WireType, MAX_MESSAGE_BODY,
};

use super::{ApiError, ApiResult, AppState};
use crate::{
    caps::{split_address, Caller, Denial},
    db::{board, messages, tables},
};

/// Map a capability denial onto HTTP, preserving the code the CLI turns into an
/// exit status. A denial is also broadcast as a `wire.denied` event so it is
/// visible in the UI rather than silent.
fn deny(s: &AppState, caller: Option<&Caller>, d: Denial) -> ApiError {
    if let Some(c) = caller {
        s.events.publish(Event::WireDenied {
            denial: WireDenial {
                from: c.node.id,
                target: match &d {
                    Denial::NoSuchNode { name } => name.clone(),
                    Denial::NoWire { to, .. } => to.clone(),
                    Denial::UnknownToken => String::new(),
                },
                required: match &d {
                    Denial::NoWire { required, .. } => *required,
                    _ => WireType::Read,
                },
                reason: d.to_string(),
                at: Timestamp::now(),
            },
        });
    }
    let status = match d {
        Denial::UnknownToken => StatusCode::UNAUTHORIZED,
        Denial::NoSuchNode { .. } => StatusCode::NOT_FOUND,
        Denial::NoWire { .. } => StatusCode::FORBIDDEN,
    };
    ApiError::new(status, d.code(), d.to_string())
}

/// Pull the node token from the Authorization header.
fn presented_token(h: &HeaderMap) -> Option<&str> {
    h.get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
}

/// Authenticate, and refuse a token belonging to a node type that has no
/// business running the CLI.
fn caller(s: &AppState, h: &HeaderMap) -> Result<Caller, ApiError> {
    let token = presented_token(h).ok_or_else(|| {
        ApiError::new(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "missing node token",
        )
    })?;
    let conn = s.db.lock().map_err(|_| ApiError::internal("db poisoned"))?;
    let c = Caller::authenticate(&conn, token).map_err(|d| deny(s, None, d))?;
    if !crate::caps::may_use_cli(c.node.node_type()) {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "wire_denied",
            format!(
                "{} {} node may not use the cli",
                c.node.node_type().article(),
                c.node.node_type()
            ),
        ));
    }
    Ok(c)
}

// --- whoami / connections / ls ---------------------------------------------

#[derive(Serialize)]
pub struct WireView {
    pub direction: &'static str,
    pub peer: String,
    pub peer_type: NodeType,
    #[serde(rename = "type")]
    pub wire_type: WireType,
    /// Plain language, so `wheel connections` reads like `yoke connections`.
    pub means: &'static str,
}

/// `GET /v1/cli/whoami`
pub async fn whoami(
    State(s): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Json<serde_json::Value>> {
    let me = caller(&s, &headers)?;
    let conn = s.db.lock().map_err(|_| ApiError::internal("db poisoned"))?;
    Ok(Json(serde_json::json!({
        "name": me.node.name,
        "id": me.node.id,
        "type": me.node.node_type().as_str(),
        "position": me.node.position,
        "wires": wire_views(&me, &conn),
    })))
}

/// `GET /v1/cli/connections`
pub async fn connections(
    State(s): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Json<serde_json::Value>> {
    let me = caller(&s, &headers)?;
    let conn = s.db.lock().map_err(|_| ApiError::internal("db poisoned"))?;
    Ok(Json(serde_json::json!({ "wires": wire_views(&me, &conn) })))
}

fn wire_views(me: &Caller, conn: &rusqlite::Connection) -> Vec<WireView> {
    let mut out = Vec::new();
    for (peer, ty) in me.reachable(conn) {
        out.push(WireView {
            direction: "out",
            peer_type: peer.node_type(),
            means: semantics(true, peer.node_type(), ty),
            peer: peer.name.into_string(),
            wire_type: ty,
        });
    }
    for (peer, ty) in me.inbound(conn) {
        out.push(WireView {
            direction: "in",
            peer_type: peer.node_type(),
            means: semantics(false, peer.node_type(), ty),
            peer: peer.name.into_string(),
            wire_type: ty,
        });
    }
    out
}

/// Same wording as the preamble, so what an agent is told at startup and what
/// `wheel connections` prints cannot disagree.
fn semantics(outgoing: bool, peer: NodeType, ty: WireType) -> &'static str {
    wheel_core::WireLine {
        outgoing,
        peer: wheel_core::NodeName::new("x").expect("literal is a valid name"),
        peer_type: peer,
        wire_type: ty,
    }
    .semantics()
}

/// `GET /v1/cli/ls` — with no target, every keyspace I can reach (§3c#7).
pub async fn ls(
    State(s): State<AppState>,
    headers: HeaderMap,
    axum::extract::Query(q): axum::extract::Query<LsQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    let me = caller(&s, &headers)?;
    let conn = s.db.lock().map_err(|_| ApiError::internal("db poisoned"))?;

    let Some(target) = q.node.as_deref() else {
        // Bare `wheel ls`: enumerate reachable keyspaces. On YOKE this was
        // operator-only, which left agents unable to discover what they could
        // touch (§3c#7).
        let entries: Vec<_> = me
            .reachable(&conn)
            .into_iter()
            .map(|(n, ty)| {
                serde_json::json!({
                    "name": n.name, "type": n.node_type().as_str(), "wire": ty.as_str()
                })
            })
            .collect();
        return Ok(Json(serde_json::json!({ "keyspaces": entries })));
    };

    let node = me
        .require(&conn, target, WireType::Read)
        .map_err(|d| deny(&s, Some(&me), d))?;
    match node.node_type() {
        NodeType::Table => {
            let keys = tables::list_keys(&conn, &node.name, q.prefix.as_deref(), MAX_KEYS, 0)
                .map_err(storage_err)?;
            Ok(Json(serde_json::json!({ "node": node.name, "keys": keys })))
        }
        NodeType::Chest => Ok(Json(serde_json::json!({ "keys": [] }))),
        other => Err(ApiError::invalid(format!("a {other} node has no keys"))),
    }
}

#[derive(Debug, Deserialize)]
pub struct LsQuery {
    pub node: Option<String>,
    pub prefix: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct NodeQuery {
    pub node: String,
}

// --- read / write ----------------------------------------------------------

/// `GET /v1/cli/read?addr=<node>[/<row>]`
pub async fn read(
    State(s): State<AppState>,
    headers: HeaderMap,
    axum::extract::Query(q): axum::extract::Query<AddrQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    let me = caller(&s, &headers)?;
    let conn = s.db.lock().map_err(|_| ApiError::internal("db poisoned"))?;
    let (name, _row) = split_address(&q.addr);

    let node = me
        .require(&conn, name, WireType::Read)
        .map_err(|d| deny(&s, Some(&me), d))?;

    match &node.config {
        wheel_core::NodeConfig::Ctx(c) => Ok(Json(serde_json::json!({
            "node": node.name, "type": "ctx", "value": c.markdown
        }))),
        wheel_core::NodeConfig::Table(cfg) => {
            let (_, row) = split_address(&q.addr);
            match row {
                Some(key) => {
                    let value = tables::get_row(&conn, &node.name, cfg, key)
                        .map_err(storage_err)?
                        .ok_or_else(|| ApiError::not_found(format!("{}/{key}", node.name)))?;
                    Ok(Json(serde_json::json!({
                        "node": node.name, "type": "table", "row": key, "value": value
                    })))
                }
                // A bare table address is the whole keyspace, paged.
                None => {
                    let rows = tables::list_rows(&conn, &node.name, cfg, q.limit(), q.offset())
                        .map_err(storage_err)?;
                    Ok(Json(serde_json::json!({
                        "node": node.name, "type": "table", "rows": rows,
                        "limit": q.limit(), "offset": q.offset()
                    })))
                }
            }
        }
        other => Err(ApiError::invalid(format!(
            "reading a {} node is not implemented yet",
            other.node_type()
        ))),
    }
}

#[derive(Debug, Deserialize)]
pub struct AddrQuery {
    pub addr: String,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub offset: Option<usize>,
}

impl AddrQuery {
    fn limit(&self) -> usize {
        self.limit.unwrap_or(MAX_KEYS).min(MAX_KEYS)
    }
    fn offset(&self) -> usize {
        self.offset.unwrap_or(0)
    }
}

/// Read ceiling for a single call (§ "Read ceilings").
const MAX_KEYS: usize = crate::db::tables::MAX_ROWS;

/// `GET /v1/cli/secret?addr=<vault>/<key>`
///
/// Wire-gated like every other read: the caller's token resolves to a node,
/// and a vault it has no read wire to is exit 3, not an empty answer.
pub async fn secret_get(
    State(s): State<AppState>,
    headers: HeaderMap,
    axum::extract::Query(q): axum::extract::Query<AddrQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    let me = caller(&s, &headers)?;
    let (name, key) = split_address(&q.addr);
    let key = key.ok_or_else(|| ApiError::invalid("secret get needs <vault>/<key>"))?;

    let vk = s.supervisor.require_vault_key().map_err(ApiError::config)?;
    let conn = s.db.lock().map_err(|_| ApiError::internal("db poisoned"))?;
    let node = me
        .require(&conn, name, WireType::Read)
        .map_err(|d| deny(&s, Some(&me), d))?;
    if !matches!(node.config, wheel_core::NodeConfig::Vault(_)) {
        return Err(ApiError::invalid(format!("{} is not a vault", node.name)));
    }

    let value = crate::vault::get(&conn, vk, node.id, key)
        .map_err(|e| ApiError::internal(e.to_string()))?
        // Exit 4 territory: the vault is reachable, the key is not there.
        .ok_or_else(|| ApiError::not_found(format!("{}/{} is not set", node.name, key)))?;

    Ok(Json(
        serde_json::json!({ "node": node.name, "key": key, "value": value }),
    ))
}

/// `GET /v1/cli/secret/keys?node=<vault>` — names only, never values.
pub async fn secret_keys(
    State(s): State<AppState>,
    headers: HeaderMap,
    axum::extract::Query(q): axum::extract::Query<NodeQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    let me = caller(&s, &headers)?;
    let conn = s.db.lock().map_err(|_| ApiError::internal("db poisoned"))?;
    let node = me
        .require(&conn, &q.node, WireType::Read)
        .map_err(|d| deny(&s, Some(&me), d))?;
    if !matches!(node.config, wheel_core::NodeConfig::Vault(_)) {
        return Err(ApiError::invalid(format!("{} is not a vault", node.name)));
    }
    let keys =
        crate::vault::list_keys(&conn, node.id).map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(Json(serde_json::json!({ "node": node.name, "keys": keys })))
}

#[derive(Debug, Deserialize)]
pub struct WriteBody {
    pub addr: String,
    pub value: String,
}

/// `POST /v1/cli/write`
pub async fn write(
    State(s): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<WriteBody>,
) -> ApiResult<Json<serde_json::Value>> {
    let me = caller(&s, &headers)?;
    let conn = s.db.lock().map_err(|_| ApiError::internal("db poisoned"))?;
    let (name, _row) = split_address(&body.addr);

    if body.value.len() > wheel_core::MAX_VALUE_BYTES {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "too_large",
            format!(
                "value is {} bytes; the limit is {}",
                body.value.len(),
                wheel_core::MAX_VALUE_BYTES
            ),
        ));
    }

    let node = me
        .require(&conn, name, WireType::Write)
        .map_err(|d| deny(&s, Some(&me), d))?;

    match node.node_type() {
        NodeType::Ctx => {
            let mut updated = node.clone();
            updated.config = wheel_core::NodeConfig::Ctx(wheel_core::CtxConfig {
                markdown: body.value,
            });
            board::update(&conn, &updated)?;
            s.events.publish(Event::BoardChanged {
                at: Timestamp::now(),
            });
            Ok(Json(
                serde_json::json!({ "node": updated.name, "ok": true }),
            ))
        }
        NodeType::Table => {
            let (_, row) = split_address(&body.addr);
            let key =
                row.ok_or_else(|| ApiError::invalid("writing a table needs <table>/<row>"))?;
            let cfg = table_config(&node)?;
            let value: serde_json::Value = serde_json::from_str(&body.value).map_err(|e| {
                ApiError::invalid(format!("a table row must be a JSON object: {e}"))
            })?;
            tables::put_row(&conn, &node.name, cfg, key, &value).map_err(storage_err)?;
            Ok(Json(
                serde_json::json!({ "node": node.name, "row": key, "ok": true }),
            ))
        }
        other => Err(ApiError::invalid(format!(
            "writing a {other} node is not implemented yet"
        ))),
    }
}

// --- tables ----------------------------------------------------------------

/// The config of a node the caller has already been authorised to reach.
fn table_config(node: &wheel_core::Node) -> ApiResult<&wheel_core::TableConfig> {
    match &node.config {
        wheel_core::NodeConfig::Table(c) => Ok(c),
        other => Err(ApiError::invalid(format!(
            "{} is a {} node, not a table",
            node.name,
            other.node_type()
        ))),
    }
}

fn storage_err(e: anyhow::Error) -> ApiError {
    // These carry the agent's own mistake -- an unknown column, a value of the
    // wrong type -- so they are the caller's to fix, not a 500.
    ApiError::invalid(e.to_string())
}

/// `POST /v1/cli/rm` — delete a table row (needs `write`).
pub async fn rm(
    State(s): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<AddrBody>,
) -> ApiResult<Json<serde_json::Value>> {
    let me = caller(&s, &headers)?;
    let conn = s.db.lock().map_err(|_| ApiError::internal("db poisoned"))?;
    let (name, row) = split_address(&body.addr);
    let row = row.ok_or_else(|| ApiError::invalid("rm needs <node>/<row>"))?;

    let node = me
        .require(&conn, name, WireType::Write)
        .map_err(|d| deny(&s, Some(&me), d))?;

    match node.node_type() {
        NodeType::Table => {
            let removed = tables::delete_row(&conn, &node.name, row).map_err(storage_err)?;
            Ok(Json(serde_json::json!({
                "node": node.name, "row": row, "removed": removed
            })))
        }
        other => Err(ApiError::invalid(format!(
            "removing from a {other} node is not implemented yet"
        ))),
    }
}

#[derive(Debug, Deserialize)]
pub struct AddrBody {
    pub addr: String,
}

#[derive(Debug, Deserialize)]
pub struct QueryBody {
    pub table: String,
    pub sql: String,
}

/// `POST /v1/cli/query` — read-only SQL scoped to ONE table.
///
/// `read` is enough: the query cannot write, and the authorizer confines it to
/// the table named in the address.
pub async fn query(
    State(s): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<QueryBody>,
) -> ApiResult<Json<serde_json::Value>> {
    let me = caller(&s, &headers)?;
    let table = {
        let conn = s.db.lock().map_err(|_| ApiError::internal("db poisoned"))?;
        let node = me
            .require(&conn, &body.table, WireType::Read)
            .map_err(|d| deny(&s, Some(&me), d))?;
        table_config(&node)?;
        tables::table_name(&node.name).map_err(storage_err)?
    };
    // The engine's own connection is NOT held across the query: user SQL runs
    // on its own read-only connection, and holding the writer would let a slow
    // query stall message delivery for everyone.
    let rows = tables::query(&s.cfg.db_path(), &table, &body.sql).map_err(storage_err)?;
    Ok(Json(serde_json::json!({ "rows": rows })))
}

// --- msg / inbox -----------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct MsgBody {
    pub to: String,
    pub body: String,
    #[serde(default)]
    pub reply_to: Option<uuid::Uuid>,
}

/// `POST /v1/cli/msg` → `{id, sha256, bytes, state}` (§3c#3).
///
/// The sender is derived from the token and never taken from the request, which
/// is what makes attribution unforgeable.
pub async fn msg(
    State(s): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<MsgBody>,
) -> ApiResult<(StatusCode, Json<MessageReceipt>)> {
    let me = caller(&s, &headers)?;

    if body.body.len() > MAX_MESSAGE_BODY {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "too_large",
            format!(
                "message body is {} bytes; the limit is {MAX_MESSAGE_BODY}",
                body.body.len()
            ),
        ));
    }

    let msg = {
        let conn = s.db.lock().map_err(|_| ApiError::internal("db poisoned"))?;
        let target = me
            .require(&conn, &body.to, WireType::Send)
            .map_err(|d| deny(&s, Some(&me), d))?;

        let from = MessageSender::Node {
            id: me.node.id,
            name: me.node.name.clone(),
            node_type: me.node.node_type(),
        };
        messages::enqueue(&conn, from, target.id, body.body.clone(), body.reply_to)
            .map_err(|e| ApiError::internal(e.to_string()))?
    };

    s.events.publish(Event::Message {
        message: msg.clone(),
    });
    // A message never starts a process (§3c#13): this only nudges an already
    // running agent to drain.
    let _ = s.supervisor.deliver(msg.to).await;

    Ok((StatusCode::ACCEPTED, Json(MessageReceipt::from(&msg))))
}

/// `GET /v1/cli/inbox` — re-read my own messages (§3c#2).
pub async fn inbox(
    State(s): State<AppState>,
    headers: HeaderMap,
    axum::extract::Query(q): axum::extract::Query<InboxQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    let me = caller(&s, &headers)?;
    let conn = s.db.lock().map_err(|_| ApiError::internal("db poisoned"))?;

    // Scoped to the caller's OWN node id, taken from the token — an agent
    // cannot read another node's inbox by asking for it.
    if let Some(id) = q.id {
        let m = messages::get(&conn, id)
            .map_err(|e| ApiError::internal(e.to_string()))?
            .filter(|m| m.to == me.node.id)
            .ok_or_else(|| ApiError::not_found(id.to_string()))?;
        return Ok(Json(serde_json::json!({ "message": m })));
    }

    let limit = q.limit.unwrap_or(50).min(10_000);
    let list: Vec<Message> = messages::inbox(&conn, me.node.id, None, limit)
        .map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(Json(serde_json::json!({ "messages": list })))
}

#[derive(Debug, Deserialize)]
pub struct InboxQuery {
    pub id: Option<uuid::Uuid>,
    pub limit: Option<u32>,
}

/// `GET /v1/cli/list` — every agent on my board (§3e parity with `yoke list`).
pub async fn list(
    State(s): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Json<serde_json::Value>> {
    let _me = caller(&s, &headers)?;
    let conn = s.db.lock().map_err(|_| ApiError::internal("db poisoned"))?;
    let agents: Vec<_> = board::list(&conn)
        .map_err(|e| ApiError::internal(e.to_string()))?
        .into_iter()
        .filter(|n| n.node_type() == NodeType::Agent)
        .map(|n| {
            let st = board::agent_state(&conn, n.id).unwrap_or_default();
            serde_json::json!({
                "name": n.name,
                "status": st.status.as_str(),
                "session_id": st.session_id,
                "hosted_on": st.hosted_on,
            })
        })
        .collect();
    Ok(Json(serde_json::json!({ "agents": agents })))
}

/// Unused today, kept so the log-stream vocabulary has one definition.
pub const CLI_LOG_STREAMS: [LogStream; 4] = [
    LogStream::Stdout,
    LogStream::Stderr,
    LogStream::Engine,
    LogStream::Transcript,
];

// --- tool nodes (§3d) -------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct ToolCallBody {
    pub node: String,
    pub op: String,
    #[serde(default)]
    pub args: serde_json::Value,
    #[serde(default)]
    pub curl: bool,
}

/// `GET /v1/cli/tool?node=<tool>` — the operations I may call, and the fields
/// I must fill.
///
/// `read` is the wire: a tool call is the agent using a capability the board
/// granted it, and the tool's own credentials are never part of what it sees.
pub async fn tool_ls(
    State(s): State<AppState>,
    headers: HeaderMap,
    axum::extract::Query(q): axum::extract::Query<NodeQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    let me = caller(&s, &headers)?;
    let conn = s.db.lock().map_err(|_| ApiError::internal("db poisoned"))?;
    let node = me
        .require(&conn, &q.node, WireType::Read)
        .map_err(|d| deny(&s, Some(&me), d))?;
    let cfg = match &node.config {
        wheel_core::NodeConfig::Tool(c) => c.clone(),
        other => {
            return Err(ApiError::invalid(format!(
                "{} is {} {} node, not a tool",
                node.name,
                other.node_type().article(),
                other.node_type()
            )))
        }
    };
    drop(conn);
    Ok(Json(serde_json::json!({
        "tool": node.name,
        "operations": crate::api::tool_routes::agent_view(node.name.as_ref(), &cfg),
    })))
}

/// `POST /v1/cli/tool` — call an operation.
pub async fn tool_call(
    State(s): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<ToolCallBody>,
) -> ApiResult<Json<serde_json::Value>> {
    let me = caller(&s, &headers)?;
    let (node, cfg) = {
        let conn = s.db.lock().map_err(|_| ApiError::internal("db poisoned"))?;
        let node = me
            .require(&conn, &body.node, WireType::Read)
            .map_err(|d| deny(&s, Some(&me), d))?;
        let cfg = match &node.config {
            wheel_core::NodeConfig::Tool(c) => c.clone(),
            other => {
                return Err(ApiError::invalid(format!(
                    "{} is {} {} node, not a tool",
                    node.name,
                    other.node_type().article(),
                    other.node_type()
                )))
            }
        };
        (node, cfg)
    };

    crate::api::tool_routes::run_operation(&s, &node, &cfg, &body.op, &body.args, body.curl)
        .await
        .map(Json)
}

/// `GET /v1/cli/mcp/tools` — the MCP tool list for the calling node.
///
/// Built here rather than in the CLI so the list reflects the caller's CURRENT
/// wires: a tool node wired after the agent started still appears, and one
/// unwired disappears, without restarting the child.
pub async fn mcp_tools(
    State(s): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Json<serde_json::Value>> {
    let me = caller(&s, &headers)?;
    let conn = s.db.lock().map_err(|_| ApiError::internal("db poisoned"))?;
    Ok(Json(serde_json::json!({
        "tools": crate::mcp::tools_for(&conn, &me),
    })))
}
