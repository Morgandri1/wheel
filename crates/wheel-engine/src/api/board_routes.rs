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

    let renamed_from = if let Some(name) = body.name {
        let was = node.name.clone();
        if was == name {
            None
        } else {
            // §4 and PROTOCOL.md's `agent_running`, which both DOCUMENTED this
            // and neither enforced. An agent's name is embedded in every peer's
            // preamble and in its own running session, so renaming a live one
            // leaves peers addressing a name that no longer exists and an agent
            // introduced as something it is not.
            //
            // Documented-but-absent is worse than missing: everything
            // downstream was written believing this held.
            let status = board::agent_state(&conn, id).unwrap_or_default().status;
            if rename_is_refused(node.node_type(), status) {
                return Err(ApiError::new(
                    StatusCode::CONFLICT,
                    "agent_running",
                    format!(
                        "{was} is {status}, and its name is in every peer's preamble and in its own session. Stop or park it first."
                    ),
                ));
            }
            node.name = name;
            Some(was)
        }
    } else {
        None
    };
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

    // A workspace is keyed by the agent's name (§3e `ws/<name>`), so a rename
    // that left it behind would orphan the tree the agent has been working in:
    // the next start materialises a fresh one and the old checkout, with any
    // uncommitted work in it, is simply somewhere the engine no longer looks.
    // Disk grows and the agent's files "vanish", which is a bad thing to
    // discover months later.
    if let Some(was) = renamed_from {
        move_workspace(
            &s.cfg.workspace_dir(was.as_str()),
            &s.cfg.workspace_dir(node.name.as_str()),
        );
    }
    s.events.publish(Event::BoardChanged {
        at: Timestamp::now(),
    });
    Ok(Json(node))
}

/// Carry an agent's working copy across a rename.
///
/// A workspace is keyed by the agent's NAME (§3e `ws/<name>`), so a rename that
/// left it behind would orphan the tree the agent has been working in: the next
/// start materialises a fresh one and the old checkout — with any uncommitted
/// work in it — is simply somewhere the engine no longer looks. Disk grows and
/// the agent's files "vanish", which is a bad thing to discover months later.
///
/// Never overwrites. If something already occupies the destination it is left
/// alone and the source is left alone: two trees an operator can inspect beat
/// one the engine chose between.
///
/// A failure here is logged, not returned. The rename itself already succeeded
/// and the board is consistent; failing the whole operation afterwards would
/// leave the caller thinking nothing happened when the node has in fact been
/// renamed.
fn move_workspace(from: &std::path::Path, to: &std::path::Path) {
    if !from.exists() || to.exists() {
        return;
    }
    if let Err(e) = std::fs::rename(from, to) {
        tracing::error!(
            from = %from.display(), to = %to.display(), error = %e,
            "renamed the node but could not move its workspace; the old checkout is orphaned"
        );
    }
}

/// May this node be renamed in the state it is in?
///
/// §4 and PROTOCOL.md both specify a 409 `agent_running` here, and neither the
/// engine nor any test enforced it — documented-but-absent, which is worse than
/// missing, because everything downstream was written believing it held.
///
/// A live agent is any holding a process: running, starting, or idle. `idle` is
/// included deliberately — it is where an agent spends most of its life, and a
/// guard omitting it would be true on paper and never fire.
fn rename_is_refused(node_type: NodeType, status: wheel_core::AgentStatus) -> bool {
    use wheel_core::AgentStatus::*;
    node_type == NodeType::Agent && matches!(status, Running | Starting | Idle)
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

#[cfg(test)]
mod rename_tests {
    use super::*;
    use wheel_core::AgentStatus::*;

    /// The states holding a process are exactly the states a rename must be
    /// refused in, and `idle` is the one that matters: it is where an agent
    /// spends most of its life, so a guard omitting it would never fire.
    #[test]
    fn a_rename_is_refused_for_every_agent_state_that_holds_a_process() {
        for live in [Running, Starting, Idle] {
            assert!(
                rename_is_refused(NodeType::Agent, live),
                "{live} holds a process and a session; renaming strands every peer addressing it"
            );
        }
        for settled in [Stopped, Parked, NeedsAuth, BudgetExhausted, Error] {
            assert!(
                !rename_is_refused(NodeType::Agent, settled),
                "{settled} has no live session; refusing here would make the board unusable"
            );
        }
    }

    fn scratch(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "wheel-mv-{tag}-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// The uncommitted work is the whole point: a rename must carry the tree,
    /// not leave the agent looking at an empty new one while its files sit
    /// under the old name.
    #[test]
    fn a_workspace_follows_its_agent_across_a_rename() {
        let root = scratch("moves");
        let from = root.join("ws/old-name");
        let to = root.join("ws/new-name");
        std::fs::create_dir_all(&from).unwrap();
        std::fs::write(from.join("WIP"), "half-finished work").unwrap();

        move_workspace(&from, &to);

        assert!(!from.exists(), "the old path must not be left behind");
        assert_eq!(
            std::fs::read_to_string(to.join("WIP")).unwrap(),
            "half-finished work",
            "the agent's uncommitted work must arrive intact"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// Never overwrite. Two trees an operator can look at beat one the engine
    /// silently chose between.
    #[test]
    fn a_move_never_overwrites_an_occupied_destination() {
        let root = scratch("occupied");
        let from = root.join("ws/a");
        let to = root.join("ws/b");
        std::fs::create_dir_all(&from).unwrap();
        std::fs::create_dir_all(&to).unwrap();
        std::fs::write(from.join("mine"), "source").unwrap();
        std::fs::write(to.join("theirs"), "destination").unwrap();

        move_workspace(&from, &to);

        assert!(from.join("mine").exists(), "the source must survive");
        assert!(
            to.join("theirs").exists(),
            "the destination must be untouched"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// An agent that never had a workspace is the ordinary case, and it must
    /// not be an error.
    #[test]
    fn a_move_with_nothing_to_move_is_a_no_op() {
        let root = scratch("absent");
        move_workspace(&root.join("ws/nope"), &root.join("ws/also-nope"));
        assert!(!root.join("ws/also-nope").exists());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn only_agents_are_guarded() {
        for t in [NodeType::Ctx, NodeType::Table, NodeType::Endpoint] {
            assert!(!rename_is_refused(t, Running));
        }
    }
}
