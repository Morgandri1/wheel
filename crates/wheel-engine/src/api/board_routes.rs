//! Board routes: `/v1/board`, `/v1/nodes`, `/v1/wires`.

use axum::{extract::State, http::StatusCode, Json};
use uuid::Uuid;
use wheel_core::{Event, Node, NodeState, NodeType, NodeWithState, Timestamp, WireSpec};

use axum::extract::Path;

use super::{ApiError, ApiResult, AppState, CreateNode, PatchNode};
use crate::db::board;

/// `GET /v1/board` → `{ nodes: NodeWithState[], project: {...} }`.
///
/// The only board read. Vault values are never included: a vault node returns
/// its `config.keys` and nothing else.
pub async fn get_board(State(s): State<AppState>) -> ApiResult<Json<serde_json::Value>> {
    let conn = s.db.lock().map_err(|_| ApiError::internal("db poisoned"))?;
    let nodes = board::list(&conn).map_err(|e| ApiError::internal(e.to_string()))?;

    let with_state: Vec<NodeWithState> = nodes
        .into_iter()
        .map(|n| {
            // `state` is present for every node and null for non-agents, so a
            // client can tell "has no state" from "not loaded".
            let state = match n.node_type() {
                NodeType::Agent => Some(NodeState::Agent(
                    board::agent_state(&conn, n.id).unwrap_or_default(),
                )),
                _ => None,
            };
            NodeWithState { node: n, state }
        })
        .collect();

    Ok(Json(serde_json::json!({
        "nodes": with_state,
        "project": { "id": s.cfg.project_id },
    })))
}

/// `POST /v1/nodes` → the created `Node`.
pub async fn create_node(
    State(s): State<AppState>,
    Json(body): Json<CreateNode>,
) -> ApiResult<(StatusCode, Json<Node>)> {
    let node = Node {
        id: Uuid::new_v4(),
        name: body.name,
        position: body.position,
        wires: Vec::new(),
        config: body.config,
    };
    let conn = s.db.lock().map_err(|_| ApiError::internal("db poisoned"))?;
    board::create_with(&conn, &node, &s.cfg.tool_allow_hosts)?;
    s.events.publish(Event::BoardChanged {
        at: Timestamp::now(),
    });
    Ok((StatusCode::CREATED, Json(node)))
}

/// `PATCH /v1/nodes/:id` — partial update of name, position and/or config.
pub async fn patch_node(
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<PatchNode>,
) -> ApiResult<Json<Node>> {
    let conn = s.db.lock().map_err(|_| ApiError::internal("db poisoned"))?;
    let mut node = board::get(&conn, id)
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| ApiError::not_found(id.to_string()))?;

    if let Some(name) = body.name {
        node.name = name;
    }
    if let Some(pos) = body.position {
        node.position = pos;
    }
    if let Some(cfg) = body.config {
        // A config patch must be re-tagged with the node's EXISTING type: a
        // PATCH may never change what kind of node this is, because the type
        // determines its wires, its storage and its capabilities.
        let tagged = serde_json::json!({ "type": node.node_type().as_str(), "config": cfg });
        node.config = serde_json::from_value(tagged)
            .map_err(|e| ApiError::invalid(format!("config does not match node type: {e}")))?;
    }

    board::update_with(&conn, &node, &s.cfg.tool_allow_hosts)?;
    s.events.publish(Event::BoardChanged {
        at: Timestamp::now(),
    });
    Ok(Json(node))
}

/// `DELETE /v1/nodes/:id` — cascades wires in both directions, plus the node's
/// rows, blobs and queued messages.
pub async fn delete_node(State(s): State<AppState>, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    let conn = s.db.lock().map_err(|_| ApiError::internal("db poisoned"))?;
    let existed = board::delete(&conn, id).map_err(|e| ApiError::internal(e.to_string()))?;
    if existed {
        s.events.publish(Event::BoardChanged {
            at: Timestamp::now(),
        });
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found(id.to_string()))
    }
}

/// `POST /v1/wires` — validated against the §3 matrix. Idempotent.
pub async fn add_wire(State(s): State<AppState>, Json(w): Json<WireSpec>) -> ApiResult<StatusCode> {
    let conn = s.db.lock().map_err(|_| ApiError::internal("db poisoned"))?;
    board::add_wire(&conn, w.from, w.to, w.wire_type, None)?;
    s.events.publish(Event::BoardChanged {
        at: Timestamp::now(),
    });
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /v1/wires`
pub async fn remove_wire(
    State(s): State<AppState>,
    Json(w): Json<WireSpec>,
) -> ApiResult<StatusCode> {
    let conn = s.db.lock().map_err(|_| ApiError::internal("db poisoned"))?;
    let existed = board::remove_wire(&conn, w.from, w.to, w.wire_type)
        .map_err(|e| ApiError::internal(e.to_string()))?;
    if existed {
        s.events.publish(Event::BoardChanged {
            at: Timestamp::now(),
        });
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found("no such wire"))
    }
}
