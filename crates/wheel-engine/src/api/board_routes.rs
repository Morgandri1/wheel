// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

//! Board routes: `/v1/board`, `/v1/nodes`, `/v1/wires`.

use axum::{extract::State, http::StatusCode, Json};
use uuid::Uuid;
use wheel_core::node::RedactCredentials;
use wheel_core::{Event, Node, NodeState, NodeType, NodeWithState, Timestamp, WireSpec};

use axum::extract::Path;
use serde::Deserialize;

use super::{ApiError, ApiResult, AppState, CreateNode, PatchNode};
use crate::db::board;

/// `GET /v1/board` → `{ nodes: NodeWithState[], project: {...} }`.
///
/// The only board read. Vault values are never included: a vault node returns
/// its `config.keys` and nothing else.
pub async fn get_board(
    State(s): State<AppState>,
    headers: axum::http::HeaderMap,
) -> ApiResult<Json<serde_json::Value>> {
    // `GET /v1/board` is a GUEST route, and the board is where a project's whole credential map
    // would otherwise be handed out in one response: vault key names, every `vault_ref` naming
    // `<vault>/<KEY>`, and every static fill's operator-typed value in plaintext. Below admin the
    // config is projected — see `wheel_core::node::RedactCredentials` for what goes and what stays.
    let tier = super::actor::tier_from_headers(&headers);
    let redact = tier < super::actor::ActorTier::Admin;

    let conn = s.db.lock().map_err(|_| ApiError::internal("db poisoned"))?;
    let nodes = board::list(&conn).map_err(|e| ApiError::internal(e.to_string()))?;

    let with_state: Vec<serde_json::Value> = nodes
        .into_iter()
        .map(|mut n| {
            // `state` is present for every node and null for non-agents, so a
            // client can tell "has no state" from "not loaded".
            let state = match n.node_type() {
                NodeType::Agent => Some(NodeState::Agent(
                    board::agent_state(&conn, n.id).unwrap_or_default(),
                )),
                _ => None,
            };
            // `redacted` is emitted so a client can say "hidden — admin only" rather than render an
            // empty key list, which reads as "this vault has none". Showing nothing and showing
            // nothing-because-you-may-not-see-it are different facts.
            let hidden = redact && n.config.has_redactable_credentials();
            if redact {
                n.config = n.config.redact_credentials();
            }
            let mut v = serde_json::to_value(NodeWithState { node: n, state })
                .unwrap_or(serde_json::Value::Null);
            if hidden {
                if let Some(obj) = v.as_object_mut() {
                    obj.insert("redacted".into(), serde_json::Value::Bool(true));
                }
            }
            v
        })
        .collect();

    Ok(Json(serde_json::json!({
        "nodes": with_state,
        "project": { "id": s.cfg.project_id },
    })))
}

/// `POST /v1/nodes` → the created `Node`.
/// Refuse a harness this build cannot actually run.
///
/// `agent_cfg.harness` selects which credentials are exported, never which
/// binary is spawned — the driver is a hardcoded `ClaudeDriver`. So without
/// this, a `codex` node is created, handed `CODEX_API_KEY`, and then `claude`
/// is spawned with it: the operator's harness choice is silently substituted
/// and nothing says so. Refusing beats running the wrong thing quietly.
fn reject_unsupported_harness(config: &wheel_core::NodeConfig) -> Result<(), ApiError> {
    if let wheel_core::NodeConfig::Agent(a) = config {
        if !crate::harness::has_driver(a.harness) {
            let h = a.harness;
            return Err(ApiError::invalid(format!(
                "harness \"{h}\" is not supported by this build (M2): there is no {h} driver, \
                 and running the node would silently spawn claude instead"
            )));
        }
    }
    Ok(())
}

pub async fn create_node(
    State(s): State<AppState>,
    Json(body): Json<CreateNode>,
) -> ApiResult<(StatusCode, Json<Node>)> {
    reject_unsupported_harness(&body.config)?;
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

#[derive(Debug, Deserialize)]
pub struct NodeContent {
    pub markdown: String,
}

/// `PUT /v1/nodes/:id/content` — replace a ctx node's markdown, and nothing else.
///
/// # Why this route exists rather than a rule about `PATCH /v1/nodes/:id`
///
/// A prompter's job is to "manage context, and prompt agents" (operator ruling, 2026-09-11), while
/// the board's *shape* — creating, deleting, rewiring, and agent config such as model, budget and
/// harness — is admin. `PATCH /v1/nodes/:id` carries `name`, `position` and `config` in one body,
/// so ctx content and agent config arrive through the same door.
///
/// Letting a prompter through that door conditionally would mean the API authorising on a request
/// *body* — parsing node config to decide permissions, duplicating engine knowledge, and deciding
/// about one thing while the engine acts on another. That is the confusion `extractor.rs` refuses
/// for `x-project-id`, and it is how a tier acquires the conditional powers the ruling forbids.
///
/// So the narrow door is a separate path, and `auth::policy` stays a pure `(method, path)` table.
/// Same idiom as `PUT /v1/vault/:id/:key`, and the same principle `table_routes` states: the
/// operator gets exactly the same box an agent does — this is `wheel write` against a ctx node,
/// minus the wire check, because the caller is not a node and has no wires.
pub async fn put_content(
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<NodeContent>,
) -> ApiResult<Json<Node>> {
    if body.markdown.len() > wheel_core::MAX_VALUE_BYTES {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "too_large",
            format!(
                "content is {} bytes; the limit is {}",
                body.markdown.len(),
                wheel_core::MAX_VALUE_BYTES
            ),
        ));
    }

    let conn = s.db.lock().map_err(|_| ApiError::internal("db poisoned"))?;
    let node = board::get(&conn, id)
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| ApiError::not_found(id.to_string()))?;

    // Only a ctx node has operator-editable content. Refusing by node *type* rather than by
    // ignoring the field keeps the route's meaning narrow: it can never become a second way to
    // patch something a prompter is not allowed to patch.
    if !matches!(node.config, wheel_core::NodeConfig::Ctx(_)) {
        return Err(ApiError::invalid(format!(
            "{} is {} {}, and only a ctx node has editable content",
            node.name,
            node.node_type().article(),
            node.node_type()
        )));
    }

    let mut updated = node;
    updated.config = wheel_core::NodeConfig::Ctx(wheel_core::CtxConfig {
        markdown: body.markdown,
    });
    board::update(&conn, &updated)?;
    s.events.publish(Event::BoardChanged {
        at: Timestamp::now(),
    });
    Ok(Json(updated))
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
    if let Some(patch) = body.config {
        // MERGE, never replace (P0). This used to overwrite `config` wholesale,
        // so a client that PATCHed only the fields it had controls for erased
        // every field it did not: workspaces, budget and idle_timeout_secs to
        // None, run_on_startup to false — 200, no warning, config nobody typed
        // silently gone. Dogfooding IS editing agents, so the destructive path
        // was the core path.
        //
        // Merged HERE rather than read-modify-write in the UI because the UI is
        // ONE caller: board-as-code import, the operator CLI, and scripts are
        // others, and "every caller must remember" is how three of this week's
        // defects happened. It is also race-free — the handler holds the single
        // writer lock across read and write, where two clients doing
        // read-modify-write would silently drop one edit.
        //
        // RFC 7386 JSON Merge Patch, so `null` can UNSET a field. A merge with
        // only "absent means unchanged" can set `budget` and never clear it.
        let mut merged = serde_json::to_value(&node.config)
            .ok()
            .and_then(|tagged| tagged.get("config").cloned())
            .unwrap_or(serde_json::Value::Null);
        merge_patch(&mut merged, &patch);

        // The merged RESULT is validated before it is stored: `config` is a
        // tagged enum, so raw JSON merging can produce a shape that is not a
        // legal config for this type. An invalid merge is a 400, never a stored
        // half-config. The type is re-applied from the EXISTING node — a PATCH
        // may never change what kind of node this is.
        let tagged = serde_json::json!({ "type": node.node_type().as_str(), "config": merged });
        node.config = serde_json::from_value(tagged)
            .map_err(|e| ApiError::invalid(format!("config does not match node type: {e}")))?;
    }

    reject_unsupported_harness(&node.config)?;
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

/// RFC 7386 JSON Merge Patch, applied in place.
///
/// Absent means unchanged, present means replaced, and explicit `null` means
/// REMOVE — that last rule is why this is 7386 rather than something simpler.
/// Without it a field can be set and never unset, so an operator could add a
/// `budget` through the UI and have no way to take it off again.
///
/// A non-object patch replaces the target outright, which is the spec and is
/// what makes nested objects merge recursively rather than clobber.
fn merge_patch(target: &mut serde_json::Value, patch: &serde_json::Value) {
    let serde_json::Value::Object(patch_map) = patch else {
        *target = patch.clone();
        return;
    };
    if !target.is_object() {
        *target = serde_json::Value::Object(serde_json::Map::new());
    }
    let map = target.as_object_mut().expect("just made it an object");
    for (k, v) in patch_map {
        if v.is_null() {
            map.remove(k);
        } else {
            merge_patch(map.entry(k.clone()).or_insert(serde_json::Value::Null), v);
        }
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
///
/// Usually `{}`; a declared-credential overlap across two vaults (028 face 5)
/// comes back as `{"warning": "..."}` on the same 200 — the wire is created
/// either way, since only a STORED clash is refused (409).
pub async fn add_wire(
    State(s): State<AppState>,
    Json(w): Json<WireSpec>,
) -> ApiResult<Json<serde_json::Value>> {
    let warning = {
        let conn = s.db.lock().map_err(|_| ApiError::internal("db poisoned"))?;
        board::add_wire(&conn, w.from, w.to, w.wire_type, None)?
    };
    s.events.publish(Event::BoardChanged {
        at: Timestamp::now(),
    });
    Ok(Json(match warning {
        Some(w) => serde_json::json!({ "warning": w }),
        None => serde_json::json!({}),
    }))
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
mod merge_tests {
    use super::*;
    use serde_json::json;

    /// The production bug, at the layer it happened: a client PATCHing only the
    /// fields it has controls for must not erase the ones it does not.
    ///
    /// This is what was destroying agent config on the live board on every
    /// save — 200, no warning, workspaces and budget gone.
    #[test]
    fn a_patch_touching_one_field_leaves_the_others_alone() {
        let mut cfg = json!({
            "harness": "claude",
            "system_prompt": "p",
            "run_on_startup": true,
            "idle_timeout_secs": 900,
            "budget": { "max_turns": 50 },
            "workspaces": [ { "path": "wheel" } ]
        });

        // What a UI with only a prompt box sends.
        merge_patch(&mut cfg, &json!({ "system_prompt": "edited" }));

        assert_eq!(cfg["system_prompt"], "edited");
        assert_eq!(cfg["run_on_startup"], true, "run_on_startup must survive");
        assert_eq!(
            cfg["idle_timeout_secs"], 900,
            "idle_timeout_secs must survive"
        );
        assert_eq!(cfg["budget"]["max_turns"], 50, "budget must survive");
        assert_eq!(
            cfg["workspaces"][0]["path"], "wheel",
            "workspaces must survive"
        );
    }

    /// Why RFC 7386 rather than "absent means unchanged" alone: without an
    /// explicit remove, a field can be SET through the UI and never cleared.
    #[test]
    fn an_explicit_null_removes_a_field_so_it_can_be_cleared() {
        let mut cfg = json!({ "system_prompt": "p", "budget": { "max_turns": 50 } });
        merge_patch(&mut cfg, &json!({ "budget": null }));
        assert!(
            cfg.get("budget").is_none(),
            "null must REMOVE, or an operator can add a budget and never take it off"
        );
        assert_eq!(
            cfg["system_prompt"], "p",
            "and it must remove only what it names"
        );
    }

    /// Nested objects merge rather than clobber — raising max_usd must not
    /// silently drop max_turns beside it.
    #[test]
    fn a_nested_object_merges_instead_of_replacing_its_siblings() {
        let mut cfg = json!({ "budget": { "max_turns": 50, "max_usd": 1.0 } });
        merge_patch(&mut cfg, &json!({ "budget": { "max_usd": 2.0 } }));
        assert_eq!(cfg["budget"]["max_usd"], 2.0);
        assert_eq!(
            cfg["budget"]["max_turns"], 50,
            "the sibling field must survive"
        );
    }

    /// Arrays are values, not collections to merge — 7386 says replace, and
    /// element-wise merging of a workspace list would be unpredictable.
    #[test]
    fn an_array_is_replaced_wholesale_not_merged_element_wise() {
        let mut cfg = json!({ "workspaces": [ { "path": "a" }, { "path": "b" } ] });
        merge_patch(&mut cfg, &json!({ "workspaces": [ { "path": "c" } ] }));
        assert_eq!(cfg["workspaces"].as_array().unwrap().len(), 1);
        assert_eq!(cfg["workspaces"][0]["path"], "c");
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

    #[test]
    fn a_codex_agent_config_is_refused_and_a_claude_one_is_not() {
        let codex = wheel_core::NodeConfig::Agent(wheel_core::AgentConfig {
            harness: wheel_core::Harness::Codex,
            ..Default::default()
        });
        let err = super::reject_unsupported_harness(&codex)
            .expect_err("a harness this build cannot run must be refused");
        let msg = err.2.clone();
        assert!(
            msg.contains("codex"),
            "the refusal must name the harness so the operator knows what to change, got: {msg}"
        );
        assert_eq!(
            err.0,
            StatusCode::BAD_REQUEST,
            "a bad config is the caller's error"
        );

        let claude = wheel_core::NodeConfig::Agent(wheel_core::AgentConfig {
            harness: wheel_core::Harness::Claude,
            ..Default::default()
        });
        assert!(
            super::reject_unsupported_harness(&claude).is_ok(),
            "the supported harness must still be accepted"
        );
    }
}

/// 028 face 5, extended to wire creation: two vaults DECLARING the same key
/// for one agent must not block the wire, but must not vanish either. Nothing
/// below this crate's `vault.rs` unit layer ever asserted on `add_wire`'s
/// `Option<String>` return before this -- every other call site in this
/// crate's own tests uses `.unwrap()` and discards it -- so this is the first
/// place the HANDLER'S response body (what a client actually receives from
/// `POST /v1/wires`) is checked at all.
#[cfg(test)]
mod wire_warning_tests {
    use super::*;
    use wheel_core::{AgentConfig, NodeConfig, Position, VaultConfig, WireType};

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

    #[tokio::test]
    async fn a_declared_only_overlap_is_a_warning_not_a_refusal() {
        let state = crate::api::test_state();
        let (agent, _v1, v2) = {
            let conn = state.db.lock().unwrap();
            let agent = mk(&conn, "agent", NodeConfig::Agent(AgentConfig::default()));
            let v1 = mk(
                &conn,
                "v1",
                NodeConfig::Vault(VaultConfig {
                    keys: vec!["ANTHROPIC_API_KEY".into()],
                }),
            );
            let v2 = mk(
                &conn,
                "v2",
                NodeConfig::Vault(VaultConfig {
                    keys: vec!["ANTHROPIC_API_KEY".into()],
                }),
            );
            board::add_wire(&conn, agent, v1, WireType::Read, None).unwrap();
            (agent, v1, v2)
        };

        let resp = add_wire(
            State(state.clone()),
            Json(WireSpec {
                from: agent,
                to: v2,
                wire_type: WireType::Read,
            }),
        )
        .await
        .expect("a declared-only overlap must not refuse the wire")
        .0;

        assert_eq!(
            resp["warning"]
                .as_str()
                .map(|s| s.contains("v1") || s.contains("v2")),
            Some(true),
            "the response must name a vault, got {resp}"
        );
        {
            let conn = state.db.lock().unwrap();
            let node = board::get(&conn, agent).unwrap().unwrap();
            assert!(
                node.wires
                    .iter()
                    .any(|w| w.to == v2 && w.wire_type == WireType::Read),
                "the wire must actually exist despite the warning"
            );
        }
    }

    #[tokio::test]
    async fn no_overlap_at_all_leaves_the_response_bare() {
        let state = crate::api::test_state();
        let (agent, v) = {
            let conn = state.db.lock().unwrap();
            let agent = mk(&conn, "agent", NodeConfig::Agent(AgentConfig::default()));
            let v = mk(
                &conn,
                "v",
                NodeConfig::Vault(VaultConfig {
                    keys: vec!["ANTHROPIC_API_KEY".into()],
                }),
            );
            (agent, v)
        };

        let resp = add_wire(
            State(state),
            Json(WireSpec {
                from: agent,
                to: v,
                wire_type: WireType::Read,
            }),
        )
        .await
        .unwrap()
        .0;

        assert!(
            resp.get("warning").is_none(),
            "no overlap means no warning: {resp}"
        );
    }
}

/// Web's follow-up to wow-agent-brief.md #6: the same budget-proximity
/// numbers `GET /v1/cli/usage` gives an agent about itself, also reachable
/// from `GET /v1/board` for the inspector, without a second per-agent fetch.
#[cfg(test)]
mod budget_status_tests {
    use super::*;
    use wheel_core::{AgentConfig, Budget, NodeConfig, Position};

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

    #[tokio::test]
    async fn an_agent_with_a_budget_carries_its_proximity_on_the_board() {
        let state = crate::api::test_state();
        let agent = {
            let conn = state.db.lock().unwrap();
            let id = mk(
                &conn,
                "agent",
                NodeConfig::Agent(AgentConfig {
                    budget: Some(Budget {
                        max_turns: Some(10),
                        max_usd: None,
                    }),
                    ..Default::default()
                }),
            );
            board::add_spend(&conn, id, 3, 0.0).unwrap();
            id
        };

        let resp = get_board(State(state)).await.unwrap().0;
        let node = resp["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|n| n["id"] == agent.to_string())
            .unwrap();
        assert_eq!(node["state"]["budget_status"]["max_turns"], 10);
        assert_eq!(node["state"]["budget_status"]["pct_of_max_turns"], 30.0);
        assert!(
            node["state"]["budget_status"].get("max_usd").is_none(),
            "an unconfigured ceiling must not appear: {node}"
        );
    }

    #[tokio::test]
    async fn an_agent_with_no_budget_carries_no_budget_status_at_all() {
        let state = crate::api::test_state();
        let agent = {
            let conn = state.db.lock().unwrap();
            mk(&conn, "agent", NodeConfig::Agent(AgentConfig::default()))
        };

        let resp = get_board(State(state)).await.unwrap().0;
        let node = resp["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|n| n["id"] == agent.to_string())
            .unwrap();
        assert!(
            node["state"].get("budget_status").is_none(),
            "no budget configured means no budget_status key at all: {node}"
        );
    }
}

/// ADVERSARY #1: `validate_config_with`'s `Ctx` size check is exercised directly by
/// `wheel-core`'s own tests, and by `POST /v1/cli/write`'s tests in `cli_routes.rs` -- but never,
/// until now, through the route that was the actual gap: `PATCH /v1/nodes/:id`. Belt and braces
/// (PM's ask, not blocking #82's merge): a REAL HTTP request through the full router, so a future
/// change that reroutes or rewires `patch_node` without going through `board::update` would fail
/// here even if `validate_config_with`'s own unit tests still passed.
#[cfg(test)]
mod ctx_patch_size_limit_tests {
    use axum::{
        body::Body,
        http::{header, Request, StatusCode},
    };
    use tower::ServiceExt;
    use uuid::Uuid;
    use wheel_core::{CtxConfig, Node, NodeConfig, Position};

    use crate::db::board;

    fn mk_ctx(conn: &rusqlite::Connection, name: &str, markdown: &str) -> Uuid {
        let n = Node::new(
            Uuid::new_v4(),
            name.parse().unwrap(),
            Position::default(),
            NodeConfig::Ctx(CtxConfig {
                markdown: markdown.to_string(),
            }),
        );
        board::create(conn, &n).unwrap();
        n.id
    }

    #[tokio::test]
    async fn patching_a_ctx_node_past_the_byte_ceiling_is_refused_over_real_http() {
        let state = crate::api::test_state();
        let secret = state.cfg.engine_secret.clone();
        let id = {
            let conn = state.db.lock().unwrap();
            mk_ctx(&conn, "notes", "small")
        };
        let app = crate::api::router(state);

        let oversized = "x".repeat(wheel_core::MAX_VALUE_BYTES + 1);
        let body = serde_json::json!({"config": {"markdown": oversized}});
        let req = Request::builder()
            .method("PATCH")
            .uri(format!("/v1/nodes/{id}"))
            .header(header::AUTHORIZATION, format!("Bearer {secret}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::BAD_REQUEST,
            "PATCH /v1/nodes/:id must refuse an oversized ctx, not just validate_config_with in isolation"
        );
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .unwrap();
        let err: wheel_core::ErrorBody = serde_json::from_slice(&bytes).unwrap();
        assert!(
            err.error.message.contains("too long"),
            "the refusal must say why: {}",
            err.error.message
        );
    }

    #[tokio::test]
    async fn patching_a_ctx_node_at_the_byte_ceiling_is_accepted_over_real_http() {
        let state = crate::api::test_state();
        let secret = state.cfg.engine_secret.clone();
        let id = {
            let conn = state.db.lock().unwrap();
            mk_ctx(&conn, "notes", "small")
        };
        let app = crate::api::router(state);

        let at_limit = "x".repeat(wheel_core::MAX_VALUE_BYTES);
        let body = serde_json::json!({"config": {"markdown": at_limit}});
        let req = Request::builder()
            .method("PATCH")
            .uri(format!("/v1/nodes/{id}"))
            .header(header::AUTHORIZATION, format!("Bearer {secret}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "the limit is inclusive, matching wheel-core's own boundary test"
        );
    }
}
