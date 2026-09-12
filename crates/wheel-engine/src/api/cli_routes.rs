// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

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
    WireType, MAX_AWAIT_SECS, MAX_MESSAGE_BODY,
};

use super::{ApiError, ApiResult, AppState};
use crate::{
    caps::{split_address, Caller, Denial},
    db::{board, messages, tables},
    supervisor::awaits::AwaitRefused,
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
            let cfg = table_config(&node)?;
            let keys = tables::list_keys(&conn, &node.name, cfg, q.prefix.as_deref(), MAX_KEYS, 0)
                .map_err(storage_err)?;
            Ok(Json(serde_json::json!({ "node": node.name, "keys": keys })))
        }
        // Not `{"keys": []}`. Chest storage is M2, and an empty list is
        // indistinguishable from a chest that really is empty — so an agent
        // asking what is in there would be told "nothing" rather than "ask
        // again after M2". Its read/write/rm siblings all answer honestly;
        // this arm was the one that lied.
        NodeType::Chest => Err(ApiError::invalid(
            "listing a chest node is not implemented yet",
        )),
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
    // The refresh token stays with the engine: an agent holding it would be a
    // second refresher of a single-use token, and a place it can be stolen
    // from. Agents already receive the access token as CLAUDE_CODE_OAUTH_TOKEN.
    if key == wheel_core::CLAUDE_OAUTH_SESSION {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "not_readable",
            format!(
                "{key} is a sign-in the engine renews; agents receive it as \
                 CLAUDE_CODE_OAUTH_TOKEN in their environment instead"
            ),
        ));
    }

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
    //
    // `{e}` prints only the outermost `.with_context(...)` layer (e.g.
    // "writing t_reports/x"), which is a caption with no subject: every
    // sqlite failure in `tables.rs` is wrapped that way, so the actual cause
    // -- "no such table: t_reports" -- never reached the caller. `{e:#}`
    // walks the whole chain.
    ApiError::invalid(format!("{e:#}"))
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
    //
    // That trade is right, and it is also the ONLY cli path where the
    // capability check and the action do not share a lock. Every other handler
    // holds the single writer connection across check AND act, so a concurrent
    // `DELETE /v1/wires` — which needs that same lock — cannot land between
    // them; the window is closed by the single-writer design rather than by a
    // transaction. Here the lock is released on purpose, so the window is real:
    // a wire revoked while a 5s query runs would otherwise still return its
    // rows.
    //
    // So the wire is re-checked before the rows are DISCLOSED. The read may
    // have happened against a capability that has since been revoked; nothing
    // is handed back unless the capability still holds at the moment of
    // disclosure, which is the guarantee that actually matters to the operator
    // who revoked it.
    let rows = tables::query(&s.cfg.db_path(), &table, &body.sql).map_err(storage_err)?;
    {
        let conn = s.db.lock().map_err(|_| ApiError::internal("db poisoned"))?;
        me.require(&conn, &body.table, WireType::Read)
            .map_err(|d| deny(&s, Some(&me), d))?;
    }
    Ok(Json(serde_json::json!({ "rows": rows })))
}

// --- msg / inbox -----------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct MsgBody {
    pub to: String,
    pub body: String,
    #[serde(default)]
    pub reply_to: Option<uuid::Uuid>,
    /// Wait up to this many seconds for the turn that consumes the message,
    /// and return how it ended. Clamped to [`wheel_core::MAX_AWAIT_SECS`].
    #[serde(default)]
    pub await_secs: Option<u64>,
    /// Send one `system` message back to me when that turn ends.
    #[serde(default)]
    pub notify: bool,
}

/// Why a wait was refused, as the caller sees it.
fn await_refused(conn: &rusqlite::Connection, r: AwaitRefused) -> ApiError {
    match r {
        AwaitRefused::Cycle { waiting, message } => {
            let name = board::get(conn, waiting)
                .ok()
                .flatten()
                .map(|n| n.name.to_string())
                .unwrap_or_else(|| waiting.to_string());
            ApiError::new(
                StatusCode::CONFLICT,
                "await_cycle",
                format!(
                    "{name} is already waiting on you (message {message}); waiting on it back \
                     would hang both turns. Finish your turn, or send without waiting."
                ),
            )
        }
        AwaitRefused::TooMany => ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "too_many_awaits",
            format!(
                "you already have {} waits open; wait on fewer at once",
                crate::supervisor::awaits::MAX_CONCURRENT_AWAITS
            ),
        ),
    }
}

/// A receipt plus how the message's turn ended: `consumed` with its `result`,
/// `error`/`undeliverable` with its `error`, or `timeout`/`pending` when it is
/// still on its way.
pub(crate) fn with_outcome(
    receipt: &MessageReceipt,
    settled: Option<&messages::Settlement>,
    waited: bool,
) -> serde_json::Value {
    let mut out = serde_json::to_value(receipt).unwrap_or_default();
    let Some(settled) = settled else {
        out["outcome"] = "undeliverable".into();
        out["error"] = "the message no longer exists; its recipient was removed".into();
        return out;
    };
    out["state"] = serde_json::json!(settled.state);
    let outcome = match (settled.is_terminal(), waited) {
        (true, _) => settled.outcome(),
        (false, true) => "timeout",
        (false, false) => "pending",
    };
    out["outcome"] = outcome.into();
    if outcome == "consumed" {
        out["result"] = serde_json::json!(settled.result);
    } else if let Some(e) = &settled.last_error {
        out["error"] = e.clone().into();
    }
    out
}

/// `POST /v1/cli/msg` → `{id, sha256, bytes, state}` (§3c#3), plus
/// `{outcome, result?, error?}` when `await_secs` is given.
///
/// The sender is derived from the token and never taken from the request, which
/// is what makes attribution unforgeable.
pub async fn msg(
    State(s): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<MsgBody>,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    let me = caller(&s, &headers)?;

    if body.notify && me.node.node_type() != NodeType::Agent {
        return Err(ApiError::invalid(
            "only an agent can be notified: nothing else has a process to deliver it to",
        ));
    }
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

    let (msg, wait) = {
        let conn = s.db.lock().map_err(|_| ApiError::internal("db poisoned"))?;
        let target = me
            .require(&conn, &body.to, WireType::Send)
            .map_err(|d| deny(&s, Some(&me), d))?;

        // Before anything is enqueued, so a refused wait sends nothing.
        let wait = match body.await_secs {
            None => None,
            Some(secs) => Some((
                secs.clamp(1, MAX_AWAIT_SECS),
                s.supervisor
                    .begin_await(me.node.id, target.id)
                    .map_err(|r| await_refused(&conn, r))?,
            )),
        };

        let from = MessageSender::Node {
            id: me.node.id,
            name: me.node.name.clone(),
            node_type: me.node.node_type(),
        };
        // `None`, always: this is the node-token plane. An agent holding a token — its own, or a
        // sibling's under the single-uid gap (ADVERSARY 037) — cannot claim to be acting for a
        // person. The header is not read here, so there is nothing to ignore.
        let msg = messages::enqueue(
            &conn,
            from,
            target.id,
            body.body.clone(),
            body.reply_to,
            None,
        )
        .map_err(|e| ApiError::internal(e.to_string()))?;
        if body.notify {
            messages::request_notification(&conn, msg.id)
                .map_err(|e| ApiError::internal(e.to_string()))?;
        }
        (msg, wait)
    };

    s.events.publish(Event::Message {
        message: msg.clone(),
    });
    // A message never starts a process (§3c#13): this only nudges an already
    // running agent to drain.
    let _ = s.supervisor.deliver(msg.to).await;

    let receipt = MessageReceipt::from(&msg);
    let Some((secs, guard)) = wait else {
        return Ok((
            StatusCode::ACCEPTED,
            Json(serde_json::to_value(&receipt).unwrap_or_default()),
        ));
    };
    guard.set_message(msg.id);
    let settled = s.supervisor.await_settlement(msg.id, secs).await;
    drop(guard);

    // ADVERSARY 046: the result was produced after the lock was released, so
    // it is disclosed only if the send wire still holds NOW.
    {
        let conn = s.db.lock().map_err(|_| ApiError::internal("db poisoned"))?;
        me.require(&conn, &body.to, WireType::Send)
            .map_err(|d| deny(&s, Some(&me), d))?;
    }
    Ok((
        StatusCode::OK,
        Json(with_outcome(&receipt, settled.as_ref(), true)),
    ))
}

#[derive(Debug, Deserialize)]
pub struct SentQuery {
    /// Optional at EXTRACTION, required in the handler: a query the extractor
    /// rejects is a 400 answered before the caller is authenticated, which
    /// breaks this realm's "no token, no answer" contract and tells an
    /// anonymous caller the route's shape.
    #[serde(default)]
    pub id: Option<uuid::Uuid>,
    /// Wait up to this many seconds for it to settle first.
    #[serde(default)]
    pub wait: Option<u64>,
}

/// `GET /v1/cli/sent?id=<id>[&wait=<secs>]` — a message I sent, and how the
/// turn that consumed it ended, result included.
///
/// Only a message whose sender is the caller, which the token decides: anyone
/// else's is `not_found`, with no hint that it exists.
pub async fn sent(
    State(s): State<AppState>,
    headers: HeaderMap,
    axum::extract::Query(q): axum::extract::Query<SentQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    let me = caller(&s, &headers)?;
    let id =
        q.id.ok_or_else(|| ApiError::invalid("sent needs the id of a message you sent"))?;
    let (receipt, peer, guard) = {
        let conn = s.db.lock().map_err(|_| ApiError::internal("db poisoned"))?;
        let msg = messages::get(&conn, id)
            .map_err(|e| ApiError::internal(e.to_string()))?
            .filter(|m| matches!(&m.from, MessageSender::Node { id, .. } if *id == me.node.id))
            .ok_or_else(|| ApiError::not_found(id.to_string()))?;
        let peer = board::get(&conn, msg.to)
            .map_err(|e| ApiError::internal(e.to_string()))?
            .ok_or_else(|| ApiError::not_found(id.to_string()))?
            .name
            .to_string();
        me.require(&conn, &peer, WireType::Send)
            .map_err(|d| deny(&s, Some(&me), d))?;
        let guard = match q.wait {
            Some(_) => Some(
                s.supervisor
                    .begin_await(me.node.id, msg.to)
                    .map_err(|r| await_refused(&conn, r))?,
            ),
            None => None,
        };
        (MessageReceipt::from(&msg), peer, guard)
    };
    if let Some(g) = &guard {
        g.set_message(receipt.id);
    }
    let secs = q.wait.map_or(0, |w| w.clamp(1, MAX_AWAIT_SECS));
    let settled = s.supervisor.await_settlement(receipt.id, secs).await;
    drop(guard);

    // As `msg`: disclosed only while the send wire still holds.
    {
        let conn = s.db.lock().map_err(|_| ApiError::internal("db poisoned"))?;
        me.require(&conn, &peer, WireType::Send)
            .map_err(|d| deny(&s, Some(&me), d))?;
    }
    Ok(Json(with_outcome(
        &receipt,
        settled.as_ref(),
        q.wait.is_some(),
    )))
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

    // Lock released above, deliberately: the action is an external HTTP call of
    // up to 30s and holding the single writer across it would stall message
    // delivery for every agent — the same reason `query` releases it.
    //
    // Which makes this the SECOND lock-releasing handler, and ADVERSARY 046 is
    // that I claimed there was only one. `query` re-checked before disclosure
    // and this did not, so an operator who revoked an agent's read wire to a
    // tool mid-call still had the response handed to the agent they had just
    // deauthorized.
    //
    // `run_operation` takes no caller identity — its own `Read` check is the
    // tool -> vault fill edge, a different wire — so the re-check has to happen
    // here, where the caller is known.
    let out =
        crate::api::tool_routes::run_operation(&s, &node, &cfg, &body.op, &body.args, body.curl)
            .await?;
    {
        let conn = s.db.lock().map_err(|_| ApiError::internal("db poisoned"))?;
        me.require(&conn, &body.node, WireType::Read)
            .map_err(|d| deny(&s, Some(&me), d))?;
    }
    Ok(Json(out))
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

/// `POST /v1/cli/ctx/clear` — an agent clearing its own context (§3 grammar).
///
/// Only its OWN: the token names the node, and there is nothing to address.
/// An agent that could clear a peer's context could erase what that peer was
/// told without leaving a trace in either transcript.
pub async fn ctx_clear(
    State(s): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Json<serde_json::Value>> {
    let me = caller(&s, &headers)?;
    if me.node.node_type() != NodeType::Agent {
        return Err(ApiError::invalid("only an agent has a context to clear"));
    }
    let status = s
        .supervisor
        .clear_context(me.node.id)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(Json(serde_json::json!({
        "node": me.node.name,
        "cleared": true,
        "status": status.as_str(),
    })))
}

/// `GET /v1/cli/usage` — an agent's own spend against its own budget.
///
/// docs/wow-agent-brief.md #6: the harness already reports turns/cost on every
/// result (§harness/claude.rs), and the engine already counts them into
/// `agent_state` to enforce `budget` (`board::budget_exceeded`) — but nothing
/// ever handed that number back to the agent itself, so it found out about a
/// limit only by hitting `budget_exhausted`. This is a read of data the engine
/// already has: no new external call, no per-turn token count (the harness
/// does not report one — see the same module), just turns/usd and, where a
/// budget is configured, how close this agent is to it.
///
/// Only its OWN: same reasoning as `ctx_clear` above, and the same reason this
/// can never leak cross-agent or cross-project data — the token names the
/// node, and there is no argument that could ask about a different one.
pub async fn usage(
    State(s): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Json<serde_json::Value>> {
    let me = caller(&s, &headers)?;
    if me.node.node_type() != NodeType::Agent {
        return Err(ApiError::invalid("only an agent has usage to report"));
    }
    let conn = s.db.lock().map_err(|_| ApiError::internal("db poisoned"))?;
    let state = board::agent_state(&conn, me.node.id).unwrap_or_default();
    let spend = state.spend.unwrap_or_default();

    let mut out = serde_json::json!({
        "turns": spend.turns,
        "usd": spend.usd,
        "status": state.status.as_str(),
    });
    // The usage window, which is a ceiling too: one the account sets rather
    // than the board. Only what the harness actually reported is shown.
    if let Some(quota) = &state.quota {
        out["quota"] = serde_json::json!(quota);
    }
    for (key, at) in [
        ("resets_at", state.resets_at),
        ("resume_at", state.resume_at),
        ("fallback_until", state.fallback_until),
    ] {
        if let Some(at) = at {
            out[key] = serde_json::json!(at);
        }
    }
    // `board::agent_state` computes `budget_status` from the same
    // `BudgetStatus::compute` this route used to duplicate inline, so this
    // and `GET /v1/board` can never disagree about what "80% of budget"
    // means. Budget is optional config (§3); an agent with none configured
    // gets its raw spend back and nothing to divide by, which is not an
    // error.
    if let Some(budget) = state.budget_status {
        if let Some(max) = budget.max_turns {
            out["max_turns"] = serde_json::json!(max);
            out["pct_of_max_turns"] = serde_json::json!(budget.pct_of_max_turns);
        }
        if let Some(max) = budget.max_usd {
            out["max_usd"] = serde_json::json!(max);
            out["pct_of_max_usd"] = serde_json::json!(budget.pct_of_max_usd);
        }
    }
    Ok(Json(out))
}

#[cfg(test)]
mod toctou_tests {
    /// ADVERSARY 046, generalised so the NEXT one is caught rather than filed.
    ///
    /// Most cli handlers hold the single writer connection across both the
    /// capability check and the action, so a concurrent `DELETE /v1/wires`
    /// cannot land between them — the window is closed by the single-writer
    /// design. Two handlers deliberately RELEASE the lock before acting,
    /// because their action is slow and holding the writer would stall message
    /// delivery for every agent: `query` runs user SQL, `tool_call` makes an
    /// external HTTP request of up to 30s.
    ///
    /// Those two must re-check the caller's wire before DISCLOSING the result.
    /// I claimed there was only one such handler and there were two; `query`
    /// re-checked and `tool_call` did not, so a revoke landing mid-call still
    /// handed the response to the agent it had just deauthorized.
    ///
    /// Asserted on the SHAPE rather than on the two names, because the fix for
    /// one instance does not protect the next: `wheel run <script>` is the same
    /// pattern and is not written yet.
    /// Handlers that release the db lock before acting, and what each owes.
    ///
    /// `true` = it discloses a result produced AFTER the lock was released, so
    /// it must re-check the caller's wire before returning that result.
    /// `false` = it releases the lock but discloses nothing derived from the
    /// post-lock work, so there is nothing to withhold.
    ///
    /// A handler that is not in this list fails the test. That is the point:
    /// the next one to do async I/O outside the lock has to be classified by a
    /// person rather than inherit whichever answer the code happened to give.
    const LOCK_RELEASING: &[(&str, bool)] = &[
        // runs user SQL on its own read-only connection, then returns rows
        ("query", true),
        // makes an external HTTP call of up to 30s, then returns the response
        ("tool_call", true),
        // with `await_secs`, waits outside the lock and then returns the
        // recipient's result: produced after the release, so re-checked.
        ("msg", true),
        // the same wait and the same disclosure, for a message already sent.
        ("sent", true),
    ];

    /// ADVERSARY 046, generalised so the NEXT one is caught rather than filed.
    ///
    /// Most cli handlers hold the single writer connection across both the
    /// capability check and the action, so a concurrent `DELETE /v1/wires`
    /// cannot land between them — the window is closed by the single-writer
    /// design. A few must release it, because their action is slow and holding
    /// the writer would stall message delivery for every agent.
    ///
    /// Those must re-check before DISCLOSING. I claimed there was one such
    /// handler and there were two: `query` re-checked, `tool_call` did not, so
    /// a revoke landing mid-call still handed the response to the agent it had
    /// just deauthorized.
    ///
    /// Asserted on the SHAPE, not on the two names, because fixing one instance
    /// does not protect the next — `wheel run <script>` is the same pattern and
    /// is not written yet.
    #[test]
    fn a_handler_that_releases_the_lock_re_checks_before_it_discloses() {
        let src = include_str!("cli_routes.rs");
        let code = src.split("#[cfg(test)]").next().unwrap_or_default();

        let mut unclassified = Vec::new();
        let mut unguarded = Vec::new();
        for chunk in code.split("pub async fn ").skip(1) {
            let name = chunk.split('(').next().unwrap_or_default().trim();
            // A lock taken at block depth (8 spaces) rather than at the top of
            // the handler (4) is one that is released before the action.
            if !chunk
                .lines()
                .any(|l| l.starts_with("        let conn = s.db.lock()"))
            {
                continue;
            }
            match LOCK_RELEASING.iter().find(|(n, _)| *n == name) {
                None => unclassified.push(name.to_string()),
                Some((_, must_recheck)) => {
                    // Counted as `.require(` because the first check is written
                    // `me\n.require(..)`; matching `me.require(` missed it and
                    // made this test read as failing when it was not.
                    if *must_recheck && chunk.matches(".require(").count() < 2 {
                        unguarded.push(name.to_string());
                    }
                }
            }
        }

        assert!(
            unclassified.is_empty(),
            "these handlers release the db lock before acting and nobody has decided whether they \
             disclose a result produced after the release: {unclassified:?}. Add them to \
             LOCK_RELEASING with true (re-check before returning) or false (nothing to withhold), \
             and say which in a comment."
        );
        assert!(
            unguarded.is_empty(),
            "these handlers release the db lock, act, and disclose the result without re-checking \
             the caller's wire, so a capability revoked mid-action still yields its data: \
             {unguarded:?}. Re-acquire the lock and `require(..)` again before returning."
        );
    }

    /// A chest must never be answered with success while its storage is
    /// unimplemented. `ls` used to return `{"keys": []}`, which an agent cannot
    /// tell apart from a chest that is genuinely empty — so it was told
    /// "nothing is in there" when the truth was "nobody has built this yet".
    /// Its `read`, `write` and `rm` siblings all say so plainly.
    ///
    /// When chest storage lands, this test is the thing that should fail, and
    /// deleting it is the right fix at that point.
    #[test]
    fn no_chest_arm_answers_with_success_while_storage_is_unimplemented() {
        let src = include_str!("cli_routes.rs");
        let production = src.split("#[cfg(test)]").next().unwrap_or_default();
        let offenders: Vec<&str> = production
            .lines()
            .filter(|l| l.contains("NodeType::Chest =>") && l.contains("Ok("))
            .collect();
        assert!(
            offenders.is_empty(),
            "a chest is being answered with a success while its storage is not implemented, so an              agent cannot tell 'empty' from 'not built yet': {offenders:?}"
        );
    }
}

#[cfg(test)]
mod storage_err_tests {
    use super::storage_err;

    /// PM, live on the wheel-dev board: a table node whose backing table went
    /// missing answered `wheel write` with `{"message":"writing t_reports/x"}`
    /// -- a caption with no subject. Every `tables.rs` function wraps its
    /// sqlite call in `.with_context(|| "verb noun")`, and `anyhow::Error`'s
    /// `{}` (what `.to_string()` used) prints ONLY that outermost layer, so
    /// the actual cause never reached the caller. `storage_err` must render
    /// the whole chain (`{:#}`), not just the caption.
    #[test]
    fn the_message_names_the_cause_the_context_was_wrapping() {
        let err =
            anyhow::anyhow!("no such table: t_reports").context("writing t_reports/2026-09-06-sdk");
        let rendered = format!("{:?}", storage_err(err));
        assert!(
            rendered.contains("writing t_reports/2026-09-06-sdk"),
            "must keep the caption: {rendered}"
        );
        assert!(
            rendered.contains("no such table: t_reports"),
            "must also name the cause, not just the caption: {rendered}"
        );
    }
}

/// Delegation that returns the reply (parity proposal §2), end to end: real
/// route handlers, a real supervisor, and `qa/harness/fake-claude` as the
/// recipient's harness.
#[cfg(test)]
pub(crate) mod await_tests {
    use super::*;
    use axum::http::HeaderValue;
    use std::sync::{Arc, Mutex};
    use wheel_core::{AgentConfig, AgentStatus, Node, NodeConfig, Position};

    pub(crate) fn fake_state(name: &str) -> (AppState, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "wheel-await-{name}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let config = dir.join("fake.json");
        std::fs::write(&config, "{}").unwrap();
        let cfg = Arc::new(crate::config::Config {
            project_id: uuid::Uuid::new_v4(),
            engine_secret: "0123456789abcdef".into(),
            vault_key: None,
            data_dir: dir.clone(),
            listen: wheel_core::ListenAddr::parse("tcp://127.0.0.1:7999").unwrap(),
            json_logs: false,
            tool_allow_hosts: Vec::new(),
            startup_deadline_secs: crate::config::DEFAULT_STARTUP_DEADLINE_SECS,
            harness_auth: crate::config::HarnessAuthPolicy::default(),
        });
        let db = Arc::new(Mutex::new(crate::db::open_memory().unwrap()));
        let events = Arc::new(crate::events::Bus::new());
        let supervisor = Arc::new(crate::supervisor::Supervisor::with_harness(
            cfg.clone(),
            db.clone(),
            events.clone(),
            Arc::new(crate::harness::fake::FakeClaude { config }),
        ));
        (
            AppState {
                cfg,
                supervisor,
                db,
                events,
                ingress_rate: Arc::default(),
                logins: Arc::default(),
            },
            dir,
        )
    }

    /// An agent node with a token, so it can call the cli routes.
    pub(crate) fn agent(s: &AppState, name: &str) -> (uuid::Uuid, HeaderMap) {
        let node = Node::new(
            uuid::Uuid::new_v4(),
            name.parse().unwrap(),
            Position::default(),
            NodeConfig::Agent(AgentConfig::default()),
        );
        let conn = s.db.lock().unwrap();
        board::create(&conn, &node).unwrap();
        let token = crate::db::tokens::mint(&conn, node.id).unwrap().plaintext;
        let mut h = HeaderMap::new();
        h.insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
        );
        (node.id, h)
    }

    fn wire(s: &AppState, from: uuid::Uuid, to: uuid::Uuid) {
        let conn = s.db.lock().unwrap();
        board::add_wire(&conn, from, to, WireType::Send, None).unwrap();
    }

    /// `deliver` starts a parked agent and never a stopped one (§3c#13).
    pub(crate) fn parked(s: &AppState, id: uuid::Uuid) {
        let conn = s.db.lock().unwrap();
        board::set_status(&conn, id, AgentStatus::Parked, None);
    }

    fn ask(to: &str, body: &str, await_secs: Option<u64>, notify: bool) -> Json<MsgBody> {
        Json(MsgBody {
            to: to.into(),
            body: body.into(),
            reply_to: None,
            await_secs,
            notify,
        })
    }

    fn messages_to(s: &AppState, to: uuid::Uuid) -> Vec<Message> {
        let conn = s.db.lock().unwrap();
        messages::inbox(&conn, to, None, 100).unwrap()
    }

    async fn stop_all(s: &AppState, ids: &[uuid::Uuid]) {
        for id in ids {
            s.supervisor.stop(*id).await.ok();
        }
    }

    #[tokio::test]
    async fn await_reply_returns_the_result_of_the_turn_that_consumed_it() {
        let (s, _dir) = fake_state("result");
        let (a, ha) = agent(&s, "a");
        let (b, _) = agent(&s, "b");
        wire(&s, a, b);
        parked(&s, b);

        let (status, Json(v)) = msg(
            State(s.clone()),
            ha,
            ask(
                "b",
                "what is six times seven <<FAKE:REPLY=forty-two>>",
                Some(60),
                false,
            ),
        )
        .await
        .unwrap();
        assert_eq!(status, StatusCode::OK);
        assert_eq!(v["outcome"], "consumed", "{v}");
        assert_eq!(v["state"], "consumed");
        assert_eq!(v["result"], "forty-two");
        assert!(v.get("error").is_none());
        // The receipt is still all there (§3c#3).
        assert_eq!(
            v["bytes"],
            "what is six times seven <<FAKE:REPLY=forty-two>>".len()
        );
        stop_all(&s, &[b]).await;
    }

    /// A timeout ends the WAIT, not the message: it is still delivered, and
    /// its answer can be collected later with `sent`.
    #[tokio::test]
    async fn await_reply_times_out_cleanly_and_the_answer_can_be_collected_later() {
        let (s, _dir) = fake_state("timeout");
        let (a, ha) = agent(&s, "a");
        let (b, _) = agent(&s, "b");
        wire(&s, a, b);
        parked(&s, b);

        let started = std::time::Instant::now();
        let (_, Json(v)) = msg(
            State(s.clone()),
            ha.clone(),
            ask("b", "<<FAKE:SLEEP=4>><<FAKE:REPLY=late>>", Some(1), false),
        )
        .await
        .unwrap();
        assert_eq!(v["outcome"], "timeout", "{v}");
        assert_ne!(v["state"], "consumed");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(4),
            "the wait must end at its own deadline"
        );

        let id: uuid::Uuid = v["id"].as_str().unwrap().parse().unwrap();
        let Json(later) = sent(
            State(s.clone()),
            ha,
            axum::extract::Query(SentQuery {
                id: Some(id),
                wait: Some(30),
            }),
        )
        .await
        .unwrap();
        assert_eq!(later["outcome"], "consumed", "{later}");
        assert_eq!(later["result"], "late");
        stop_all(&s, &[b]).await;
    }

    #[tokio::test]
    async fn a_turn_that_failed_is_reported_as_error_not_consumed() {
        let (s, _dir) = fake_state("error");
        let (a, ha) = agent(&s, "a");
        let (b, _) = agent(&s, "b");
        wire(&s, a, b);
        parked(&s, b);
        let (_, Json(v)) = msg(
            State(s.clone()),
            ha,
            ask("b", "<<FAKE:ERROR=it broke>>", Some(60), false),
        )
        .await
        .unwrap();
        assert_eq!(v["outcome"], "error", "{v}");
        assert_eq!(v["error"], "it broke");
        assert!(v.get("result").is_none());
        stop_all(&s, &[b]).await;
    }

    /// Exactly one `system` message, threaded to the original, and none for a
    /// message that did not ask.
    #[tokio::test]
    async fn notify_delivers_exactly_one_system_message_to_the_sender() {
        let (s, _dir) = fake_state("notify");
        let (a, ha) = agent(&s, "a");
        let (b, _) = agent(&s, "b");
        wire(&s, a, b);
        parked(&s, b);

        let (status, Json(receipt)) = msg(
            State(s.clone()),
            ha.clone(),
            ask("b", "<<FAKE:REPLY=all done>>", None, true),
        )
        .await
        .unwrap();
        assert_eq!(status, StatusCode::ACCEPTED);
        let id: uuid::Uuid = receipt["id"].as_str().unwrap().parse().unwrap();
        let (_, Json(quiet)) = msg(
            State(s.clone()),
            ha,
            ask("b", "<<FAKE:REPLY=no news>>", None, false),
        )
        .await
        .unwrap();
        let quiet_id: uuid::Uuid = quiet["id"].as_str().unwrap().parse().unwrap();

        for m in [id, quiet_id] {
            let done = s.supervisor.await_settlement(m, 60).await.unwrap();
            assert!(done.is_terminal());
        }
        let notes: Vec<Message> = messages_to(&s, a)
            .into_iter()
            .filter(|m| m.from == MessageSender::System)
            .collect();
        assert_eq!(notes.len(), 1, "exactly one notification: {notes:?}");
        assert_eq!(
            notes[0].reply_to,
            Some(id),
            "threaded to the message it is about"
        );
        assert!(
            notes[0].body.contains("to b finished: consumed"),
            "{}",
            notes[0].body
        );
        assert!(notes[0].body.contains("all done"));
        assert!(!notes[0].body.contains("no news"));
        stop_all(&s, &[b]).await;
    }

    /// A waits on B; B, in its turn, asks A back. Without the wait-for graph
    /// both turns hang until their timeouts. With it B is refused at once,
    /// nothing is sent, and A's own wait still ends by itself.
    #[tokio::test]
    async fn two_agents_awaiting_each_other_do_not_deadlock() {
        let (s, _dir) = fake_state("cycle");
        let (a, ha) = agent(&s, "a");
        let (b, hb) = agent(&s, "b");
        wire(&s, a, b);
        wire(&s, b, a);
        // B is stopped, so A's message is never delivered: A waits for real.
        let a_waits = tokio::spawn(msg(State(s.clone()), ha, ask("b", "hello", Some(3), false)));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while messages_to(&s, b).is_empty() && std::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }

        let started = std::time::Instant::now();
        let refused = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            msg(
                State(s.clone()),
                hb,
                ask("a", "back at you", Some(60), false),
            ),
        )
        .await
        .expect("B's ask must be answered at once, not hang")
        .expect_err("a cycle of waits must be refused");
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
        assert_eq!(refused.0, StatusCode::CONFLICT);
        assert_eq!(refused.1, "await_cycle");
        assert!(
            refused.2.contains("a is already waiting on you"),
            "{}",
            refused.2
        );
        assert!(messages_to(&s, a).is_empty(), "a refused ask sends nothing");

        let (_, Json(v)) = a_waits.await.unwrap().unwrap();
        assert_eq!(v["outcome"], "timeout");
        assert!(
            s.supervisor.begin_await(b, a).is_ok(),
            "once A's wait is over, B may ask A"
        );
    }

    /// ADVERSARY 046 for results: the wire is re-checked at disclosure.
    #[tokio::test]
    async fn a_wire_revoked_while_waiting_withholds_the_result() {
        let (s, _dir) = fake_state("revoke");
        let (a, ha) = agent(&s, "a");
        let (b, _) = agent(&s, "b");
        wire(&s, a, b);
        parked(&s, b);
        let waiting = tokio::spawn(msg(
            State(s.clone()),
            ha,
            ask(
                "b",
                "<<FAKE:SLEEP=2>><<FAKE:REPLY=the secret plan>>",
                Some(60),
                false,
            ),
        ));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while !messages_to(&s, b)
            .iter()
            .any(|m| m.state == wheel_core::MessageState::Delivered)
        {
            assert!(std::time::Instant::now() < deadline, "never delivered");
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        {
            let conn = s.db.lock().unwrap();
            board::remove_wire(&conn, a, b, WireType::Send).unwrap();
        }
        let err = waiting
            .await
            .unwrap()
            .expect_err("the result must be withheld");
        assert_eq!(err.0, StatusCode::FORBIDDEN);
        assert!(!err.2.contains("the secret plan"));
        stop_all(&s, &[b]).await;
    }

    #[tokio::test]
    async fn sent_answers_only_the_node_that_sent_the_message() {
        let (s, _dir) = fake_state("sent");
        let (a, ha) = agent(&s, "a");
        let (b, hb) = agent(&s, "b");
        wire(&s, a, b);
        wire(&s, b, a);
        let (_, Json(receipt)) = msg(State(s.clone()), ha.clone(), ask("b", "hi", None, false))
            .await
            .unwrap();
        let id: uuid::Uuid = receipt["id"].as_str().unwrap().parse().unwrap();
        let q = || {
            axum::extract::Query(SentQuery {
                id: Some(id),
                wait: None,
            })
        };

        let Json(mine) = sent(State(s.clone()), ha, q()).await.unwrap();
        assert_eq!(mine["outcome"], "pending", "B is stopped: {mine}");
        let err = sent(State(s.clone()), hb, q())
            .await
            .expect_err("the recipient did not send it");
        assert_eq!(err.0, StatusCode::NOT_FOUND);
    }

    /// The realm answers "no token" before it answers "bad request": an
    /// extractor that rejects first would 400 an anonymous caller, which both
    /// breaks the realm's contract and describes the route to someone who
    /// presented nothing.
    #[tokio::test]
    async fn sent_is_unauthorized_before_it_is_invalid() {
        let (s, _dir) = fake_state("sent-auth");
        let empty = axum::extract::Query(SentQuery {
            id: None,
            wait: None,
        });
        let err = sent(State(s.clone()), HeaderMap::new(), empty)
            .await
            .expect_err("no token, no answer");
        assert_eq!(err.0, StatusCode::UNAUTHORIZED);

        // ...and once a token IS presented, the missing id is a plain 400.
        let (_a, ha) = agent(&s, "a");
        let err = sent(
            State(s),
            ha,
            axum::extract::Query(SentQuery {
                id: None,
                wait: None,
            }),
        )
        .await
        .expect_err("a message id is required");
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn only_an_agent_can_ask_to_be_notified() {
        let (s, _dir) = fake_state("notify-script");
        let (b, _) = agent(&s, "b");
        let script = Node::new(
            uuid::Uuid::new_v4(),
            "cron".parse().unwrap(),
            Position::default(),
            NodeConfig::Script(wheel_core::ScriptConfig {
                language: wheel_core::ScriptLanguage::Python,
                source: "print('hi')".into(),
                timeout_secs: None,
            }),
        );
        let token = {
            let conn = s.db.lock().unwrap();
            board::create(&conn, &script).unwrap();
            board::add_wire(&conn, script.id, b, WireType::Send, None).unwrap();
            crate::db::tokens::mint(&conn, script.id).unwrap().plaintext
        };
        let mut h = HeaderMap::new();
        h.insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
        );
        let err = msg(State(s.clone()), h, ask("b", "x", None, true))
            .await
            .expect_err("a script has no process to notify");
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
        assert!(messages_to(&s, b).is_empty());
    }
}

/// docs/wow-agent-brief.md #6: nothing above `db::board::budget_exceeded`'s own
/// unit layer ever called this handler before this, so a route that dropped a
/// field or leaked another agent's spend would have shipped green.
#[cfg(test)]
mod usage_tests {
    use super::*;
    use axum::http::HeaderValue;
    use wheel_core::{AgentConfig, Budget, Node, NodeConfig, Position};

    fn agent_with_token(state: &AppState, config: AgentConfig) -> (uuid::Uuid, HeaderMap) {
        let node = Node::new(
            uuid::Uuid::new_v4(),
            "agent".parse().unwrap(),
            Position::default(),
            NodeConfig::Agent(config),
        );
        let id = node.id;
        let token = {
            let conn = state.db.lock().unwrap();
            board::create(&conn, &node).unwrap();
            crate::db::tokens::mint(&conn, id).unwrap().plaintext
        };
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
        );
        (id, headers)
    }

    /// **The agent-token plane ignores the actor header entirely.**
    ///
    /// This is the rule that keeps an agent from claiming to act for a person. It matters most
    /// under the single-uid gap (ADVERSARY 037), where an agent can read a *sibling's* token file
    /// and send as that node: the message is then misattributed to the wrong node, which is bad —
    /// but it carries no `on_behalf_of` at all, so it cannot be laundered into "a human asked for
    /// this". Missing attribution rather than forged attribution, by design.
    #[tokio::test]
    async fn a_cli_send_can_never_assert_an_actor() {
        let state = super::super::test_state();
        let (from, mut headers) = agent_with_token(&state, AgentConfig::default());
        let to = {
            let node = Node::new(
                uuid::Uuid::new_v4(),
                "peer".parse().unwrap(),
                Position::default(),
                NodeConfig::Agent(AgentConfig::default()),
            );
            let id = node.id;
            let conn = state.db.lock().unwrap();
            board::create(&conn, &node).unwrap();
            board::add_wire(&conn, from, id, wheel_core::WireType::Send, None).unwrap();
            id
        };

        // A well-formed actor header, exactly as the API would set it on the control plane.
        headers.insert(
            "x-wheel-actor-id",
            HeaderValue::from_static("3f2504e0-4f89-11d3-9a0c-0305e82c3301"),
        );

        let (_status, Json(receipt)) = msg(
            State(state.clone()),
            headers,
            Json(MsgBody {
                to: "peer".into(),
                body: "hello".into(),
                reply_to: None,
            }),
        )
        .await
        .expect("the send is accepted");

        let conn = state.db.lock().unwrap();
        let stored = crate::db::messages::get(&conn, receipt.id)
            .unwrap()
            .expect("the row exists");
        assert_eq!(
            stored.on_behalf_of, None,
            "an agent asserted an actor on the node-token plane"
        );
        assert!(!stored.envelope().contains("on_behalf_of"));
        let _ = to;
    }

    /// No budget configured: raw spend comes back, with nothing fabricated to
    /// divide it by.
    #[tokio::test]
    async fn no_budget_reports_raw_spend_and_no_percentages() {
        let state = crate::api::test_state();
        let (id, headers) = agent_with_token(&state, AgentConfig::default());
        {
            let conn = state.db.lock().unwrap();
            board::add_spend(&conn, id, 3, 0.5).unwrap();
        }

        let resp = usage(State(state), headers).await.unwrap().0;
        assert_eq!(resp["turns"], 3);
        assert_eq!(resp["usd"], 0.5);
        assert!(
            resp.get("max_turns").is_none() && resp.get("pct_of_max_turns").is_none(),
            "no budget means nothing to compare against: {resp}"
        );
    }

    /// Both ceilings configured: both percentages must be present, each
    /// computed against its OWN ceiling, not the other one's.
    #[tokio::test]
    async fn both_ceilings_report_independent_percentages() {
        let state = crate::api::test_state();
        let (id, headers) = agent_with_token(
            &state,
            AgentConfig {
                budget: Some(Budget {
                    max_turns: Some(50),
                    max_usd: Some(10.0),
                }),
                ..Default::default()
            },
        );
        {
            let conn = state.db.lock().unwrap();
            board::add_spend(&conn, id, 5, 2.5).unwrap();
        }

        let resp = usage(State(state), headers).await.unwrap().0;
        assert_eq!(resp["turns"], 5);
        assert_eq!(resp["usd"], 2.5);
        assert_eq!(resp["max_turns"], 50);
        assert_eq!(resp["pct_of_max_turns"], 10.0);
        assert_eq!(resp["max_usd"], 10.0);
        assert_eq!(resp["pct_of_max_usd"], 25.0);
    }

    /// Only one ceiling set: the other must not appear at all, not as a null
    /// or a fabricated zero.
    #[tokio::test]
    async fn one_configured_ceiling_does_not_invent_the_other() {
        let state = crate::api::test_state();
        let (id, headers) = agent_with_token(
            &state,
            AgentConfig {
                budget: Some(Budget {
                    max_turns: Some(4),
                    max_usd: None,
                }),
                ..Default::default()
            },
        );
        {
            let conn = state.db.lock().unwrap();
            board::add_spend(&conn, id, 2, 9.99).unwrap();
        }

        let resp = usage(State(state), headers).await.unwrap().0;
        assert_eq!(resp["pct_of_max_turns"], 50.0);
        assert!(
            resp.get("max_usd").is_none() && resp.get("pct_of_max_usd").is_none(),
            "an unconfigured ceiling must not appear at all: {resp}"
        );
    }

    /// A non-agent caller (a script, per `may_use_cli`) has no budget/spend
    /// concept — this must be a clear error, not a silently empty report.
    #[tokio::test]
    async fn a_script_caller_is_refused_not_given_an_empty_report() {
        let state = crate::api::test_state();
        let node = Node::new(
            uuid::Uuid::new_v4(),
            "script".parse().unwrap(),
            Position::default(),
            NodeConfig::Script(wheel_core::ScriptConfig {
                language: wheel_core::ScriptLanguage::Python,
                source: "print('hi')".into(),
                timeout_secs: None,
            }),
        );
        let token = {
            let conn = state.db.lock().unwrap();
            board::create(&conn, &node).unwrap();
            crate::db::tokens::mint(&conn, node.id).unwrap().plaintext
        };
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
        );

        let err = usage(State(state), headers)
            .await
            .expect_err("a script has no usage to report");
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
    }

    /// A zero ceiling is a degenerate config, not a crash: `100.0%`, not a
    /// divide-by-zero `NaN` that `serde_json` would silently turn into `null`.
    #[test]
    fn a_zero_ceiling_reports_100_percent_rather_than_dividing_by_zero() {
        let status = wheel_core::BudgetStatus::compute(
            wheel_core::Spend { turns: 5, usd: 0.0 },
            Some(wheel_core::Budget {
                max_turns: Some(0),
                max_usd: None,
            }),
        )
        .unwrap();
        assert_eq!(status.pct_of_max_turns, Some(100.0));
    }

    /// The refresh token stays with the engine. An agent holding one would be
    /// a second refresher of a single-use token — and a place to steal it
    /// from. What an agent gets is the access token, in its environment.
    ///
    /// Mutation-checked: drop the refusal and `wheel secret get` hands an
    /// agent the whole login, refresh token included.
    #[tokio::test]
    async fn an_agent_cannot_read_the_refresh_token_of_a_vaulted_login() {
        use axum::http::HeaderValue;
        let state = crate::api::test_state();
        let (agent, token) = {
            let conn = state.db.lock().unwrap();
            let worker = wheel_core::Node::new(
                uuid::Uuid::new_v4(),
                "worker".parse().unwrap(),
                wheel_core::Position::default(),
                wheel_core::NodeConfig::Agent(wheel_core::AgentConfig::default()),
            );
            crate::db::board::create(&conn, &worker).unwrap();
            let agent = worker.id;
            let vault = wheel_core::Node::new(
                uuid::Uuid::new_v4(),
                "anthropic".parse().unwrap(),
                wheel_core::Position::default(),
                wheel_core::NodeConfig::Vault(wheel_core::VaultConfig { keys: vec![] }),
            );
            crate::db::board::create(&conn, &vault).unwrap();
            crate::db::board::add_wire(&conn, agent, vault.id, WireType::Read, None).unwrap();
            let vk = state.supervisor.vault_key().unwrap();
            crate::vault::put(
                &conn,
                vk,
                vault.id,
                wheel_core::CLAUDE_OAUTH_SESSION,
                r#"{"claudeAiOauth":{"accessToken":"sk-ant-oat01-a","refreshToken":"sk-ant-ort01-secret"}}"#,
            )
            .unwrap();
            crate::vault::put(&conn, vk, vault.id, "OTHER", "an ordinary secret").unwrap();
            (
                agent,
                crate::db::tokens::mint(&conn, agent).unwrap().plaintext,
            )
        };
        let _ = agent;
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
        );

        let err = secret_get(
            State(state.clone()),
            headers.clone(),
            axum::extract::Query(AddrQuery {
                addr: format!("anthropic/{}", wheel_core::CLAUDE_OAUTH_SESSION),
                limit: None,
                offset: None,
            }),
        )
        .await
        .expect_err("an agent must not be able to read the login");
        assert_eq!(err.0, StatusCode::FORBIDDEN);
        assert_eq!(err.1, "not_readable");

        // An ordinary secret in the same vault still works, so this is the one
        // key that is refused and not the wire.
        let ok = secret_get(
            State(state),
            headers,
            axum::extract::Query(AddrQuery {
                addr: "anthropic/OTHER".into(),
                limit: None,
                offset: None,
            }),
        )
        .await
        .expect("the wire still grants everything else");
        assert_eq!(ok.0["value"], "an ordinary secret");
    }
}
