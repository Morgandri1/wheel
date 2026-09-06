//! Tool-node routes (§3d, §4).
//!
//! Import is a PREVIEW first: the engine parses, the UI shows what it found,
//! and only then does a node exist. The engine is the only parser, so what the
//! preview shows and what the node holds cannot disagree.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use uuid::Uuid;

use super::{ApiError, ApiResult, AppState};
use crate::{
    db::board,
    tools::{execute, import},
};

#[derive(Debug, serde::Deserialize)]
pub struct ImportBody {
    /// Accept that this re-import drops fills an operator pinned.
    #[serde(default)]
    pub allow_unpin: bool,
    /// Omitted means "work it out": every format announces itself.
    #[serde(default)]
    pub format: Option<wheel_core::ToolFormat>,
    pub raw: String,
}

/// `POST /v1/tools/import` — parse a document, create nothing.
pub async fn preview(Json(body): Json<ImportBody>) -> ApiResult<Json<serde_json::Value>> {
    let got =
        import::import(&body.raw, body.format).map_err(|e| ApiError::invalid(e.to_string()))?;
    Ok(Json(serde_json::json!({
        "format": got.format,
        "base_url": got.base_url,
        "operations": got.operations,
    })))
}

fn tool_node(s: &AppState, id: Uuid) -> ApiResult<(wheel_core::Node, wheel_core::ToolConfig)> {
    let conn = s.db.lock().map_err(|_| ApiError::internal("db poisoned"))?;
    let node = board::get(&conn, id)
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| ApiError::not_found(id.to_string()))?;
    match &node.config {
        wheel_core::NodeConfig::Tool(c) => Ok((node.clone(), c.clone())),
        other => Err(ApiError::invalid(format!(
            "not a tool node (it is {} {})",
            other.node_type().article(),
            other.node_type()
        ))),
    }
}

/// `POST /v1/tools/:id/import` — re-import into an existing node.
///
/// Diffs by `method+path` and KEEPS the fills already configured (§3d rule 5).
/// Re-importing a spec must not silently hand fields back to the agent that an
/// operator had pinned to a vault: that would turn a routine spec refresh into
/// a credential the agent can now set.
pub async fn reimport(
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<ImportBody>,
) -> ApiResult<Json<serde_json::Value>> {
    let (node, cfg) = tool_node(&s, id)?;
    let got =
        import::import(&body.raw, body.format).map_err(|e| ApiError::invalid(e.to_string()))?;

    let m = merge_operations(&cfg.operations, got.operations);

    // PM: never demote a pinned fill to `agent` without an explicit reset. A
    // pin is the confinement for a credential the agent must never see, and a
    // routine spec refresh is not consent to remove one.
    if !m.unpinned.is_empty() && !body.allow_unpin {
        let detail: Vec<String> = m
            .unpinned
            .iter()
            .map(|u| format!("{}.{} ({})", u.op, u.param, u.was))
            .collect();
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "would_unpin",
            format!(
                "this spec no longer has {}, so {} pinned to the board would become \
                 agent-fillable. Re-pin on the new field names, or resend with allow_unpin \
                 to accept that.",
                detail.join(", "),
                if m.unpinned.len() == 1 {
                    "a field"
                } else {
                    "fields"
                }
            ),
        ));
    }

    let (merged, added, removed) = (m.operations, m.added, m.removed);
    let unpinned = m.unpinned;

    let base_url = if got.base_url.is_empty() {
        cfg.base_url.clone()
    } else {
        got.base_url
    };
    let updated = wheel_core::ToolConfig {
        kind: cfg.kind,
        source: wheel_core::ToolSource {
            format: got.format,
            raw: body.raw,
            imported_at: wheel_core::Timestamp::now(),
        },
        base_url,
        operations: merged,
    };

    {
        let conn = s.db.lock().map_err(|_| ApiError::internal("db poisoned"))?;
        let mut n = node.clone();
        n.config = wheel_core::NodeConfig::Tool(updated.clone());
        board::update(&conn, &n)?;
    }
    s.events.publish(wheel_core::Event::BoardChanged {
        at: wheel_core::Timestamp::now(),
    });

    Ok(Json(serde_json::json!({
        "operations": updated.operations,
        "added": added,
        "removed": removed,
        "unpinned": unpinned,
    })))
}

/// Fold a freshly parsed spec into the operations a node already has.
///
/// Matched by `method+path` (§3d rule 5), and the FILLS SURVIVE. That is the
/// security-relevant half: re-importing a spec must not hand a field back to
/// the agent that an operator had pinned to a vault or a fixed value, or a
/// routine refresh silently becomes "the agent can now set the API key".
///
/// Returns `(merged, added ids, removed ids)`. Removals are reported rather
/// than applied quietly: an operator who loses an operation should hear it
/// from the import, not from an agent's failed call.
pub fn merge_operations(
    existing: &[wheel_core::ToolOperation],
    fresh: Vec<wheel_core::ToolOperation>,
) -> Merged {
    let mut merged = Vec::new();
    let mut added = Vec::new();
    let mut unpinned = Vec::new();

    for mut op in fresh {
        match existing.iter().find(|old| same_operation(old, &op)) {
            Some(old) => {
                // Identity and configuration survive; shape follows the spec.
                op.id = old.id.clone();
                op.enabled = old.enabled;
                for p in op.params.iter_mut() {
                    if let Some(prev) = old.params.iter().find(|q| same_param(q, p)) {
                        p.fill = prev.fill.clone();
                    }
                }
                // A pin the fresh spec has nowhere to put. ADVERSARY 024: a
                // RENAMED parameter has no counterpart, so it silently kept
                // the fresh default of `agent` and the operator's vault pin
                // vanished — with no add/remove signal, because the operation
                // itself still matched. That is a credential slot handed to
                // the agent by a routine spec refresh.
                for prev in old.params.iter().filter(|q| is_pinned(&q.fill)) {
                    if !op.params.iter().any(|p| same_param(prev, p)) {
                        unpinned.push(Unpinned {
                            op: old.id.clone(),
                            param: prev.name.clone(),
                            was: fill_mode_name(&prev.fill).into(),
                        });
                    }
                }
            }
            None => added.push(op.id.clone()),
        }
        merged.push(op);
    }

    let mut removed = Vec::new();
    for old in existing {
        if merged.iter().any(|n| same_operation(old, n)) {
            continue;
        }
        removed.push(old.id.clone());
        // A vanished operation takes its pins with it. It IS reported as
        // removed, but the pin is the part that matters.
        for prev in old.params.iter().filter(|q| is_pinned(&q.fill)) {
            unpinned.push(Unpinned {
                op: old.id.clone(),
                param: prev.name.clone(),
                was: fill_mode_name(&prev.fill).into(),
            });
        }
    }

    Merged {
        operations: merged,
        added,
        removed,
        unpinned,
    }
}

/// A fill an operator deliberately took away from the agent.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Unpinned {
    pub op: String,
    pub param: String,
    /// `vault` or `static`.
    pub was: String,
}

#[derive(Debug)]
pub struct Merged {
    pub operations: Vec<wheel_core::ToolOperation>,
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub unpinned: Vec<Unpinned>,
}

fn is_pinned(f: &wheel_core::Fill) -> bool {
    matches!(
        f.mode,
        wheel_core::FillMode::Vault | wheel_core::FillMode::Static
    )
}

fn fill_mode_name(f: &wheel_core::Fill) -> &'static str {
    match f.mode {
        wheel_core::FillMode::Vault => "vault",
        wheel_core::FillMode::Static => "static",
        wheel_core::FillMode::Hidden => "hidden",
        wheel_core::FillMode::Agent => "agent",
    }
}

/// Two operations are the same one if they address the same thing.
///
/// Method and path are compared case-insensitively with a trailing slash
/// ignored (ADVERSARY 024, the weaker variant): an upstream that normalises
/// `/pets` to `/pets/` is a cosmetic change, and treating it as a different
/// operation would reset every fill on the board and report it as an
/// add/remove churn nobody can read.
fn same_operation(a: &wheel_core::ToolOperation, b: &wheel_core::ToolOperation) -> bool {
    a.method == b.method && normalise(&a.path) == normalise(&b.path)
}

fn normalise(path: &str) -> String {
    let p = path.trim_end_matches('/').to_ascii_lowercase();
    if p.is_empty() {
        "/".into()
    } else {
        p
    }
}

/// Two params are the same field if they are in the same place with the same
/// name. Location is part of the key because `id` in the path and `id` in the
/// query are different fields, and matching on name alone would copy one
/// field's pin onto the other.
fn same_param(a: &wheel_core::ToolParam, b: &wheel_core::ToolParam) -> bool {
    a.location == b.location && a.name.eq_ignore_ascii_case(&b.name)
}

/// `GET /v1/tools/:id/ops` — exactly what an agent would see.
///
/// Only `agent`-mode fields (§3d rule 1). This is the same projection the MCP
/// input schema uses, so the UI's "what can the agent do" and the agent's own
/// view cannot drift.
pub async fn ops(
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    let (node, cfg) = tool_node(&s, id)?;
    Ok(Json(serde_json::json!({
        "tool": node.name,
        "operations": agent_view(node.name.as_ref(), &cfg),
    })))
}

/// The operations an agent can call, with only the fields it may supply.
pub fn agent_view(tool: &str, cfg: &wheel_core::ToolConfig) -> Vec<serde_json::Value> {
    cfg.operations
        .iter()
        .filter(|o| o.enabled)
        .map(|o| {
            let mut properties = serde_json::Map::new();
            let mut required = Vec::new();
            for p in o.agent_params() {
                let mut schema = p
                    .schema
                    .clone()
                    .unwrap_or(serde_json::json!({"type": "string"}));
                if let (Some(obj), Some(d)) = (schema.as_object_mut(), p.description.as_ref()) {
                    obj.entry("description")
                        .or_insert_with(|| serde_json::json!(d));
                }
                properties.insert(p.name.clone(), schema);
                if p.required {
                    required.push(p.name.clone());
                }
            }
            serde_json::json!({
                "id": o.id,
                "name": o.mcp_name(tool),
                "method": o.method.as_str(),
                "path": o.path,
                "summary": o.summary,
                "input_schema": {
                    "type": "object",
                    "properties": properties,
                    "required": required,
                },
            })
        })
        .collect()
}

#[derive(Debug, serde::Deserialize)]
pub struct CallBody {
    pub op: String,
    #[serde(default)]
    pub args: serde_json::Value,
    /// Render the equivalent curl instead of sending anything.
    #[serde(default)]
    pub dry_run: bool,
}

/// `POST /v1/tools/:id/call` — the operator's test call, as the operator.
pub async fn call(
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<CallBody>,
) -> ApiResult<Json<serde_json::Value>> {
    let (node, cfg) = tool_node(&s, id)?;
    let outcome = run_operation(&s, &node, &cfg, &body.op, &body.args, body.dry_run).await?;
    Ok(Json(outcome))
}

/// Shared by the operator route and the agent's `/v1/cli/tool` path, so a
/// call made either way resolves its fills the same way.
pub async fn run_operation(
    s: &AppState,
    node: &wheel_core::Node,
    cfg: &wheel_core::ToolConfig,
    op_id: &str,
    args: &serde_json::Value,
    dry_run: bool,
) -> ApiResult<serde_json::Value> {
    let op = cfg
        .operations
        .iter()
        .find(|o| o.id == op_id)
        .ok_or_else(|| ApiError::not_found(format!("no operation {op_id:?} on {}", node.name)))?;

    let vault_values = resolve_vault_fills(s, node, op)?;
    let prepared = execute::build_request(cfg, op, args, &vault_values)
        .map_err(|e| ApiError::invalid(e.to_string()))?;

    if dry_run {
        // Masked: the whole point is that this can be pasted somewhere.
        return Ok(serde_json::json!({ "curl": execute::curl_for(&prepared) }));
    }

    let started = std::time::Instant::now();
    let result = execute::send(
        &prepared,
        execute::Allowlist {
            targets: &s.cfg.tool_allow_hosts,
        },
    )
    .await;
    let (status, bytes) = match &result {
        Ok(o) => (o.status, o.bytes),
        Err(_) => (0u16, 0usize),
    };
    // Logged without the resolved values (§3d rule 6): what was called, how it
    // went, and how big — never what it was called with.
    tracing::info!(
        tool = %node.name,
        op = op_id,
        status,
        bytes,
        duration_ms = started.elapsed().as_millis() as u64,
        "tool call"
    );

    let outcome = result
        .map_err(|e| ApiError::new(StatusCode::BAD_GATEWAY, "tool_error", format!("{e:#}")))?;
    Ok(serde_json::json!({
        "status": outcome.status,
        "headers": outcome.headers,
        "body": outcome.body,
        "duration_ms": outcome.duration_ms,
        "bytes": outcome.bytes,
    }))
}

/// Resolve every `vault` fill this operation needs — and refuse if the tool
/// has no wire to the vault it names.
///
/// The wire is the capability here as everywhere else (§3d: `tool → vault` is
/// a real wire in the matrix). A tool that could read any vault by naming it
/// would make the board's wiring decorative.
fn resolve_vault_fills(
    s: &AppState,
    node: &wheel_core::Node,
    op: &wheel_core::ToolOperation,
) -> ApiResult<std::collections::HashMap<String, String>> {
    let mut out = std::collections::HashMap::new();
    let refs: Vec<String> = op.vault_refs().map(str::to_string).collect();
    if refs.is_empty() {
        return Ok(out);
    }

    let vk = s.supervisor.require_vault_key().map_err(ApiError::config)?;
    let conn = s.db.lock().map_err(|_| ApiError::internal("db poisoned"))?;
    for r in refs {
        let (vault_name, key) = wheel_core::Fill::parse_vault_ref(&r)
            .ok_or_else(|| ApiError::invalid(format!("malformed vault ref {r:?}")))?;
        let vault = board::get_by_name(&conn, vault_name)
            .map_err(|e| ApiError::internal(e.to_string()))?
            .ok_or_else(|| ApiError::not_found(format!("no vault named {vault_name:?}")))?;
        if vault.node_type() != wheel_core::NodeType::Vault {
            return Err(ApiError::invalid(format!("{vault_name} is not a vault")));
        }
        if !node.has_wire(
            vault.id,
            wheel_core::WireType::Read,
            wheel_core::NodeType::Vault,
        ) {
            return Err(ApiError::new(
                StatusCode::FORBIDDEN,
                "wire_denied",
                format!(
                    "no wire from {} to {vault_name} (need: read) — wire the tool to the vault",
                    node.name
                ),
            ));
        }
        let value = crate::vault::get(&conn, vk, vault.id, key)
            .map_err(|e| ApiError::internal(e.to_string()))?
            .ok_or_else(|| ApiError::not_found(format!("{vault_name} has no key {key:?}")))?;
        out.insert(r, value);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wheel_core::{Fill, FillMode, ParamLocation, ToolMethod, ToolOperation, ToolParam};

    fn param(name: &str, fill: Fill) -> ToolParam {
        ToolParam {
            name: name.into(),
            location: ParamLocation::Header,
            required: false,
            description: None,
            schema: None,
            fill,
        }
    }

    fn vault(r: &str) -> Fill {
        Fill {
            mode: FillMode::Vault,
            value: None,
            vault_ref: Some(r.into()),
        }
    }

    fn op(id: &str, path: &str, params: Vec<ToolParam>) -> ToolOperation {
        ToolOperation {
            id: id.into(),
            method: ToolMethod::Get,
            path: path.into(),
            summary: None,
            enabled: true,
            params,
        }
    }

    /// The one that matters. Re-importing a spec must not hand a field back to
    /// the agent that an operator had pinned to a vault — a routine refresh
    /// would otherwise silently become "the agent can now set the API key".
    #[test]
    fn a_reimport_keeps_the_fills_an_operator_configured() {
        let existing = vec![op(
            "listPets",
            "/pets",
            vec![
                param("Authorization", vault("creds/KEY")),
                param(
                    "X-Env",
                    Fill {
                        mode: FillMode::Static,
                        value: Some("prod".into()),
                        vault_ref: None,
                    },
                ),
                param("limit", Fill::agent()),
            ],
        )];
        // The spec is re-fetched: same operation, and every field is an
        // agent field again, as a fresh import always produces.
        let fresh = vec![op(
            "list_pets_renamed_upstream",
            "/pets",
            vec![
                param("Authorization", Fill::agent()),
                param("X-Env", Fill::agent()),
                param("limit", Fill::agent()),
            ],
        )];

        let m = merge_operations(&existing, fresh);
        assert!(m.added.is_empty());
        assert!(m.removed.is_empty());
        assert!(
            m.unpinned.is_empty(),
            "nothing was renamed: {:?}",
            m.unpinned
        );
        assert_eq!(m.operations.len(), 1);
        let merged = m.operations;

        let o = &merged[0];
        // The id an agent addresses does not change under it.
        assert_eq!(o.id, "listPets");
        let find = |n: &str| o.params.iter().find(|p| p.name == n).unwrap();
        assert_eq!(find("Authorization").fill.mode, FillMode::Vault);
        assert_eq!(
            find("Authorization").fill.vault_ref.as_deref(),
            Some("creds/KEY")
        );
        assert_eq!(find("X-Env").fill.mode, FillMode::Static);
        assert_eq!(find("X-Env").fill.value.as_deref(), Some("prod"));
        assert_eq!(find("limit").fill.mode, FillMode::Agent);
    }

    /// ADVERSARY 024, their exact PoC. My earlier test covered an OP-ID
    /// rename with unchanged param names — not a PARAM rename, which is the
    /// case that actually loses the pin. Upstream renames `Authorization` to
    /// `authorization`, the operation still matches on method+path, so
    /// nothing appears in added/removed and the credential slot quietly
    /// becomes agent-fillable.
    #[test]
    fn a_renamed_parameter_cannot_silently_lose_its_pin() {
        let existing = vec![op(
            "getData",
            "/data",
            vec![param("Authorization", vault("prod-keys/API_KEY"))],
        )];
        // A case-only rename is the same header by HTTP's rules, so it keeps
        // the pin rather than being reported.
        let m = merge_operations(
            &existing,
            vec![op(
                "getData",
                "/data",
                vec![param("authorization", Fill::agent())],
            )],
        );
        assert!(
            m.unpinned.is_empty(),
            "a case-only header rename is the same field: {:?}",
            m.unpinned
        );
        assert_eq!(m.operations[0].params[0].fill.mode, FillMode::Vault);

        // A genuine rename has nowhere to put the pin, and MUST be reported
        // rather than defaulting the replacement to agent in silence.
        let m = merge_operations(
            &existing,
            vec![op(
                "getData",
                "/data",
                vec![param("Auth-Token", Fill::agent())],
            )],
        );
        assert_eq!(m.added, Vec::<String>::new(), "the op still matches");
        assert_eq!(m.removed, Vec::<String>::new());
        assert_eq!(m.unpinned.len(), 1, "the pin loss must be reported");
        assert_eq!(m.unpinned[0].op, "getData");
        assert_eq!(m.unpinned[0].param, "Authorization");
        assert_eq!(m.unpinned[0].was, "vault");
    }

    /// Same name, different place, is a different field — copying one pin
    /// onto the other would put a credential somewhere nobody put it.
    #[test]
    fn a_pin_does_not_move_between_locations_that_share_a_name() {
        let mut in_header = param("id", vault("creds/KEY"));
        in_header.location = ParamLocation::Header;
        let mut in_query = param("id", Fill::agent());
        in_query.location = ParamLocation::Query;

        let m = merge_operations(
            &[op("x", "/x", vec![in_header])],
            vec![op("x", "/x", vec![in_query])],
        );
        assert_eq!(m.operations[0].params[0].fill.mode, FillMode::Agent);
        assert_eq!(m.unpinned.len(), 1, "the header pin has nowhere to go");
    }

    /// The weaker variant they noted: an upstream that normalises `/pets` to
    /// `/pets/` is a cosmetic change. Treating it as a different operation
    /// would reset every fill on the board and report churn nobody can read.
    #[test]
    fn a_cosmetic_path_change_does_not_churn_every_fill() {
        let existing = vec![op(
            "listPets",
            "/pets",
            vec![param("Authorization", vault("creds/KEY"))],
        )];
        for cosmetic in ["/pets/", "/Pets", "/PETS/"] {
            let m = merge_operations(
                &existing,
                vec![op(
                    "listPets",
                    cosmetic,
                    vec![param("Authorization", Fill::agent())],
                )],
            );
            assert!(m.added.is_empty(), "{cosmetic} read as a new operation");
            assert!(m.removed.is_empty(), "{cosmetic}");
            assert!(m.unpinned.is_empty(), "{cosmetic} lost the pin");
            assert_eq!(m.operations[0].params[0].fill.mode, FillMode::Vault);
        }
        // ...but a genuinely different path is genuinely different.
        let m = merge_operations(&existing, vec![op("listPets", "/animals", vec![])]);
        assert_eq!(m.added, vec!["listPets"]);
        assert_eq!(m.removed, vec!["listPets"]);
        assert_eq!(m.unpinned.len(), 1, "the vanished op took a pin with it");
    }

    /// A disabled operation stays disabled: re-enabling one an operator turned
    /// off is the same class of mistake as un-pinning a fill.
    #[test]
    fn a_reimport_does_not_re_enable_an_operation_that_was_turned_off() {
        let mut off = op("dangerous", "/wipe", vec![]);
        off.enabled = false;
        let m = merge_operations(&[off], vec![op("dangerous", "/wipe", vec![])]);
        assert!(!m.operations[0].enabled);
    }

    #[test]
    fn new_and_vanished_operations_are_both_reported() {
        let existing = vec![op("stays", "/a", vec![]), op("goes", "/b", vec![])];
        let fresh = vec![op("stays", "/a", vec![]), op("arrives", "/c", vec![])];

        let m = merge_operations(&existing, fresh);
        assert_eq!(m.added, vec!["arrives"]);
        assert_eq!(m.removed, vec!["goes"]);
        // The removed one is NOT silently carried forward, and not silently
        // dropped either — it is named so the operator decides.
        assert_eq!(m.operations.len(), 2);
        assert!(m.operations.iter().all(|o| o.id != "goes"));
    }

    /// A field the spec no longer has cannot keep a fill: there is nothing to
    /// fill. But one that comes BACK must not inherit a stale pin either — it
    /// is matched by name, so this pins that behaviour deliberately.
    #[test]
    fn a_field_that_returns_gets_its_old_fill_back_by_name() {
        let existing = vec![op(
            "x",
            "/x",
            vec![param("Authorization", vault("creds/KEY"))],
        )];
        // Spec drops it: the params are gone AND the loss of the pin is
        // reported rather than silently accepted.
        let m = merge_operations(&existing, vec![op("x", "/x", vec![])]);
        assert!(m.operations[0].params.is_empty());
        assert_eq!(m.unpinned.len(), 1);
        assert_eq!(m.unpinned[0].param, "Authorization");
        assert_eq!(m.unpinned[0].was, "vault");
        // ...and brings it back: the operator's pin applies again rather than
        // the field arriving as an agent field.
        let m = merge_operations(
            &existing,
            vec![op("x", "/x", vec![param("Authorization", Fill::agent())])],
        );
        assert_eq!(m.operations[0].params[0].fill.mode, FillMode::Vault);
        assert!(m.unpinned.is_empty());
    }

    /// §3d rule 1: an agent sees ONLY agent-mode fields. This projection is
    /// what `wheel tool ls` and the MCP input schema are both built from, so
    /// a leak here is a leak in both.
    #[test]
    fn the_agent_view_hides_every_field_the_agent_does_not_own() {
        let cfg = wheel_core::ToolConfig {
            kind: wheel_core::ToolKind::Http,
            source: wheel_core::ToolSource {
                format: wheel_core::ToolFormat::Manual,
                raw: String::new(),
                imported_at: wheel_core::Timestamp::now(),
            },
            base_url: "https://api.example.com".into(),
            operations: vec![op(
                "send",
                "/send",
                vec![
                    param("Authorization", vault("creds/KEY")),
                    param(
                        "X-Env",
                        Fill {
                            mode: FillMode::Static,
                            value: Some("prod-secret".into()),
                            vault_ref: None,
                        },
                    ),
                    param(
                        "X-Gone",
                        Fill {
                            mode: FillMode::Hidden,
                            value: None,
                            vault_ref: None,
                        },
                    ),
                    param("text", Fill::agent()),
                ],
            )],
        };

        let view = agent_view("mailer", &cfg);
        let rendered = serde_json::to_string(&view).unwrap();
        assert_eq!(view.len(), 1);
        assert_eq!(view[0]["name"], "mailer__send");

        let props = view[0]["input_schema"]["properties"].as_object().unwrap();
        assert!(props.contains_key("text"));
        for hidden in ["Authorization", "X-Env", "X-Gone"] {
            assert!(
                !props.contains_key(hidden),
                "{hidden} is visible: {rendered}"
            );
        }
        // ...and no trace of the values either, anywhere in the payload.
        assert!(!rendered.contains("creds/KEY"), "{rendered}");
        assert!(!rendered.contains("prod-secret"), "{rendered}");
    }

    #[test]
    fn a_disabled_operation_is_not_offered_to_an_agent() {
        let mut o = op("nope", "/nope", vec![]);
        o.enabled = false;
        let cfg = wheel_core::ToolConfig {
            kind: wheel_core::ToolKind::Http,
            source: wheel_core::ToolSource {
                format: wheel_core::ToolFormat::Manual,
                raw: String::new(),
                imported_at: wheel_core::Timestamp::now(),
            },
            base_url: "https://api.example.com".into(),
            operations: vec![o],
        };
        assert!(agent_view("t", &cfg).is_empty());
    }
}
