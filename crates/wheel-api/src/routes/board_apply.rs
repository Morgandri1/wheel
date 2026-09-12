// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

//! `POST /v1/projects/{id}/board/apply` — realise a builder-emitted board.
//!
//! The logic lives in `crate::apply`, which is pure and knows nothing about HTTP. This is the
//! wiring: read the current board, validate, and either preview or execute.
//!
//! Status codes carry the success-shape invariant rather than leaving it to the body:
//!
//! - `200` — everything asked for landed, or a `dry_run` preview.
//! - `422` — the board was refused and NOTHING was created. Every refusal is listed, each naming
//!   the node or wire at fault.
//! - `207` — a partial apply. Some steps landed and some did not, and the body names both. It is
//!   deliberately not `200`: a caller that branches on 2xx-means-fine would otherwise report a
//!   half-applied board as a success, which is exactly the invariant this route exists to keep.

use crate::apply::{
    execute, validate, ApplyPolicy, ApplyReport, BoardClient, EmittedBoard, EmittedNode,
    ExistingBoard, ExistingNode, Plan, Refusal, WireRef,
};
use crate::auth::extractor::ProjectScope;
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;
use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use wheel_core::WireType;

#[derive(Debug, Deserialize)]
pub struct ApplyRequest {
    /// Read as arbitrary JSON, so a board that cannot be parsed is a REFUSAL naming the reason
    /// rather than a framework 422 with a plain-text body the confirm step cannot render.
    pub board: serde_json::Value,
    /// Plan only: say what applying would do, and change nothing.
    #[serde(default)]
    pub dry_run: bool,
    /// Allow the board to MODIFY nodes that already exist. Off unless asked for.
    ///
    /// A builder-emitted board that merely mentions an existing node would otherwise change it, and
    /// "the LLM named it" is not the user's consent. Left off, such a board is refused and the
    /// refusal names every node it would have touched — which is what the confirm step shows before
    /// anyone opts in.
    #[serde(default)]
    pub allow_patch: bool,
    /// Allow the board to WIRE nodes that already exist. Off unless asked for.
    ///
    /// Separate consent from `allow_patch`: a wire IS a capability, so attaching one to an existing
    /// node changes what it can do or what can reach it without editing it. Left off, such a board
    /// is refused with `wire_touches_existing_node`, naming the wire and which endpoint already
    /// exists — which is what the confirm step shows as "this will wire these existing nodes".
    #[serde(default)]
    pub allow_wire: bool,
    /// Allow the board to REMOVE nodes. Off unless asked for, and asked for separately from
    /// everything else: removing a node destroys what it holds — a table's rows, a vault's
    /// secrets, an agent's messages and logs — and that is not the same decision as editing one.
    #[serde(default)]
    pub allow_delete: bool,
    /// Allow the board to REMOVE wires. Off unless asked for. Reversible, unlike a node, but it
    /// takes a capability away, so it is still the user's call rather than the builder's.
    #[serde(default)]
    pub allow_unwire: bool,
    /// The digest of the plan the user actually confirmed.
    ///
    /// Required for an apply that removes anything: the board can change between the preview and
    /// the press — another tab, an agent, the builder run again — and a deletion recomputed
    /// against a board nobody looked at is not the deletion anyone consented to.
    #[serde(default)]
    pub expect_plan: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct PlanPreview {
    pub create_nodes: Vec<String>,
    pub patch_nodes: Vec<String>,
    /// Structured, not formatted: the confirm step draws these on a canvas.
    pub create_wires: Vec<WireRef>,
    /// What each patch actually changes, so the confirm step can say which fields — and which
    /// whole lists get replaced, since RFC 7386 has no element-wise array merge.
    pub patch_details: Vec<PatchDetail>,
    /// Removals, kept as their own kinds so the UI can show them apart from everything else.
    pub delete_wires: Vec<WireRef>,
    pub delete_nodes: Vec<DeletePreview>,
}

#[derive(Debug, Serialize)]
pub struct PatchDetail {
    pub name: String,
    pub fields: Vec<String>,
    pub replaced_arrays: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct DeletePreview {
    pub name: String,
    #[serde(rename = "type")]
    pub node_type: wheel_core::NodeType,
    /// Wires that go with the node. A consequence of the deletion, not a second decision.
    pub wires: Vec<WireRef>,
}

impl From<&Plan> for PlanPreview {
    fn from(p: &Plan) -> Self {
        Self {
            create_nodes: p.create_nodes.iter().map(|n| n.name.clone()).collect(),
            patch_nodes: p.patch_nodes.iter().map(|n| n.name.clone()).collect(),
            create_wires: p.create_wires.iter().map(WireRef::of_emitted).collect(),
            patch_details: p
                .patch_nodes
                .iter()
                .map(|n| PatchDetail {
                    name: n.name.clone(),
                    fields: n.fields.clone(),
                    replaced_arrays: n.replaced_arrays.clone(),
                })
                .collect(),
            delete_wires: p.delete_wires.iter().map(WireRef::of_emitted).collect(),
            delete_nodes: p
                .delete_nodes
                .iter()
                .map(|n| DeletePreview {
                    name: n.name.clone(),
                    node_type: n.node_type,
                    wires: n.wires.clone(),
                })
                .collect(),
        }
    }
}

/// What consent would unblock this refusal set, as a ready-made prompt rather than something the
/// caller has to derive.
///
/// Web's point, and it is right: a refusal that is correct but leaves the user to work out what to
/// do is worse than a bare error, because it looks actionable. For an "improve an existing board"
/// proposal, touching existing nodes IS the request — the builder cannot route around it, so
/// "ask the builder to fix it and try again" sends the user in a circle. What they actually need is
/// the exact list of what they would be agreeing to, and the flag that says yes.
///
/// Absent when nothing here is a consent problem: an illegal wire is not something the user can
/// consent their way past, and offering a toggle for it would be a lie.
#[derive(Debug, Default, Serialize)]
struct Consent {
    /// Existing nodes whose config the board would change. Unblocked by `allow_patch`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    would_modify: Vec<String>,
    /// Wires that would attach to an existing node. Unblocked by `allow_wire`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    would_wire: Vec<WireRef>,
    /// Nodes that would be REMOVED, with what each one destroys. Unblocked by `allow_delete`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    would_delete: Vec<DeleteConsent>,
    /// Wires that would be removed. Unblocked by `allow_unwire`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    would_unwire: Vec<WireRef>,
    /// The flags that, together, would let this exact board through.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    grant: Vec<&'static str>,
}

impl Consent {
    fn of(refusals: &[Refusal]) -> Option<Self> {
        let mut c = Consent::default();
        for r in refusals {
            match r {
                Refusal::PatchNotPermitted { name } => c.would_modify.push(name.clone()),
                Refusal::WireTouchesExistingNode {
                    from,
                    to,
                    wire_type,
                    ..
                } => c.would_wire.push(WireRef {
                    from: from.clone(),
                    to: to.clone(),
                    wire_type: *wire_type,
                }),
                Refusal::DeleteNotPermitted { name, node_type } => {
                    c.would_delete.push(DeleteConsent {
                        name: name.clone(),
                        node_type: *node_type,
                        destroys: destroys(*node_type),
                    })
                }
                Refusal::UnwireNotPermitted {
                    from,
                    to,
                    wire_type,
                } => c.would_unwire.push(WireRef {
                    from: from.clone(),
                    to: to.clone(),
                    wire_type: *wire_type,
                }),
                // Everything else is a board the user cannot consent their way out of.
                _ => return None,
            }
        }
        if !c.would_modify.is_empty() {
            c.grant.push("allow_patch");
        }
        if !c.would_wire.is_empty() {
            c.grant.push("allow_wire");
        }
        if !c.would_delete.is_empty() {
            c.grant.push("allow_delete");
        }
        if !c.would_unwire.is_empty() {
            c.grant.push("allow_unwire");
        }
        if c.grant.is_empty() {
            None
        } else {
            Some(c)
        }
    }
}

/// A node the board would remove, and what removing it costs.
#[derive(Debug, Serialize)]
struct DeleteConsent {
    name: String,
    #[serde(rename = "type")]
    node_type: wheel_core::NodeType,
    /// Said plainly, because "delete node" does not read as "lose the rows".
    destroys: &'static str,
}

/// What is gone for good when this node is. Written per type rather than as one warning, because
/// a ctx node and a vault are not the same loss and a single sentence for both teaches people to
/// skim it.
fn destroys(node_type: wheel_core::NodeType) -> &'static str {
    use wheel_core::NodeType as T;
    match node_type {
        T::Table => "its rows are dropped and cannot be recovered",
        T::Vault => "the secrets it holds are destroyed",
        T::Chest => "the files it holds are destroyed",
        T::Agent => "its messages, logs and stored credential go with it",
        T::Ctx => "the markdown it injects is lost",
        T::Script => "its source is lost",
        T::Endpoint => "its public URL stops answering",
        T::Mcp | T::Tool => "the agents wired to it lose that capability",
    }
}

/// The engine, over HTTP, with the host bearer the proxy uses.
///
/// `pub(crate)`: reused by `routes::instantiate`, which applies a template's board against the
/// project it just created via the exact same engine calls this route makes — no second client.
pub(crate) struct HttpBoardClient {
    http: reqwest::Client,
    base: String,
    bearer: String,
}

impl HttpBoardClient {
    pub(crate) fn new(state: &AppState, project: &Uuid) -> Self {
        Self {
            http: state.http.clone(),
            base: state.engine_base_url(project),
            bearer: format!("Bearer {}", state.cfg.host_secret.expose()),
        }
    }

    /// The engine's error body if it sent one, else the status. Passed through rather than
    /// summarised: the apply report names the step, and the engine's own words say why.
    async fn failure(resp: reqwest::Response) -> String {
        let status = resp.status();
        match resp.text().await {
            Ok(body) if !body.trim().is_empty() => format!("engine returned {status}: {body}"),
            _ => format!("engine returned {status}"),
        }
    }
}

#[async_trait::async_trait]
impl BoardClient for HttpBoardClient {
    async fn create_node(&self, node: &EmittedNode) -> Result<Uuid, String> {
        let body = serde_json::json!({
            "name": node.name,
            "position": node.position,
        });
        let mut body = body.as_object().cloned().unwrap_or_default();
        // `config` is flattened on the wire: {"name":…, "type":…, "config":{…}}.
        if let Ok(serde_json::Value::Object(cfg)) = serde_json::to_value(&node.config) {
            body.extend(cfg);
        }
        let resp = self
            .http
            .post(format!("{}/v1/nodes", self.base))
            .header("Authorization", &self.bearer)
            .json(&serde_json::Value::Object(body))
            .send()
            .await
            .map_err(|e| format!("could not reach the engine: {e}"))?;
        if !resp.status().is_success() {
            return Err(Self::failure(resp).await);
        }
        #[derive(Deserialize)]
        struct Created {
            id: Uuid,
        }
        let created: Created = resp
            .json()
            .await
            .map_err(|e| format!("the engine's reply was not a node: {e}"))?;
        Ok(created.id)
    }

    async fn patch_config(&self, id: Uuid, config: &serde_json::Value) -> Result<(), String> {
        let resp = self
            .http
            .patch(format!("{}/v1/nodes/{id}", self.base))
            .header("Authorization", &self.bearer)
            .json(config)
            .send()
            .await
            .map_err(|e| format!("could not reach the engine: {e}"))?;
        if !resp.status().is_success() {
            return Err(Self::failure(resp).await);
        }
        Ok(())
    }

    async fn add_wire(&self, from: Uuid, to: Uuid, wire_type: WireType) -> Result<(), String> {
        let resp = self
            .http
            .post(format!("{}/v1/wires", self.base))
            .header("Authorization", &self.bearer)
            .json(&serde_json::json!({"from": from, "to": to, "type": wire_type}))
            .send()
            .await
            .map_err(|e| format!("could not reach the engine: {e}"))?;
        if !resp.status().is_success() {
            return Err(Self::failure(resp).await);
        }
        Ok(())
    }

    async fn delete_node(&self, id: Uuid) -> Result<(), String> {
        let resp = self
            .http
            .delete(format!("{}/v1/nodes/{id}", self.base))
            .header("Authorization", &self.bearer)
            .send()
            .await
            .map_err(|e| format!("could not reach the engine: {e}"))?;
        if !resp.status().is_success() {
            return Err(Self::failure(resp).await);
        }
        Ok(())
    }

    async fn delete_wire(&self, from: Uuid, to: Uuid, wire_type: WireType) -> Result<(), String> {
        let resp = self
            .http
            .delete(format!("{}/v1/wires", self.base))
            .header("Authorization", &self.bearer)
            .json(&serde_json::json!({"from": from, "to": to, "type": wire_type}))
            .send()
            .await
            .map_err(|e| format!("could not reach the engine: {e}"))?;
        if !resp.status().is_success() {
            return Err(Self::failure(resp).await);
        }
        Ok(())
    }
}

/// Read the current board into the shape the apply step validates against.
async fn read_board(client: &HttpBoardClient) -> ApiResult<ExistingBoard> {
    let resp = client
        .http
        .get(format!("{}/v1/board", client.base))
        .header("Authorization", &client.bearer)
        .send()
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("could not reach the engine: {e}")))?;
    if !resp.status().is_success() {
        let status = resp.status();
        return Err(ApiError::Internal(anyhow::anyhow!(
            "the engine would not describe the board: {status}"
        )));
    }
    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("the board was not json: {e}")))?;

    let mut existing = ExistingBoard::default();
    let mut by_id: std::collections::HashMap<Uuid, String> = std::collections::HashMap::new();
    for node in body["nodes"].as_array().into_iter().flatten() {
        let (Some(name), Some(id)) = (
            node["name"].as_str(),
            node["id"].as_str().and_then(|s| s.parse::<Uuid>().ok()),
        ) else {
            continue;
        };
        let Ok(node_type) = serde_json::from_value(node["type"].clone()) else {
            continue;
        };
        by_id.insert(id, name.to_string());
        existing.nodes.insert(
            name.to_string(),
            ExistingNode {
                id,
                node_type,
                // What the node holds NOW. Without it a patch is a rewrite, and a field the board
                // never mentioned would be reset to whatever a typed default happens to be.
                config: node["config"].clone(),
            },
        );
    }
    // Wires are stored on the source node as outgoing, addressed by id; the apply step compares by
    // name, so they are translated here rather than in the pure logic.
    for node in body["nodes"].as_array().into_iter().flatten() {
        let Some(from) = node["name"].as_str() else {
            continue;
        };
        for wire in node["wires"].as_array().into_iter().flatten() {
            let Some(to) = wire["to"]
                .as_str()
                .and_then(|s| s.parse::<Uuid>().ok())
                .and_then(|id| by_id.get(&id))
            else {
                continue;
            };
            if let Ok(wire_type) = serde_json::from_value(wire["type"].clone()) {
                existing
                    .wires
                    .push((from.to_string(), to.clone(), wire_type));
            }
        }
    }
    Ok(existing)
}

pub async fn apply_board(
    State(state): State<AppState>,
    scope: ProjectScope,
    Json(req): Json<ApplyRequest>,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    // A board that cannot be read is refused in the same shape as one that is illegal, so the
    // confirm step has something to show either way.
    let board = match crate::apply::read_board(req.board) {
        Ok(board) => board,
        Err(refusal) => return Ok(refused(vec![refusal])),
    };

    let client = HttpBoardClient::new(&state, &scope.project.id);
    let existing = read_board(&client).await?;

    let policy = ApplyPolicy {
        allow_patch: req.allow_patch,
        allow_wire: req.allow_wire,
        allow_delete: req.allow_delete,
        allow_unwire: req.allow_unwire,
    };
    let plan = match validate(&board, &existing, policy) {
        Ok(plan) => plan,
        Err(refusals) => return Ok(refused(refusals)),
    };

    let digest = plan.digest();
    if req.dry_run {
        return Ok((
            StatusCode::OK,
            Json(serde_json::json!({
                "applied": false,
                "plan": PlanPreview::from(&plan),
                "plan_digest": digest,
            })),
        ));
    }

    // Destroying anything takes the plan the user SAW, not whatever this recomputed. A board that
    // moved underneath is answered with the new plan and nothing applied.
    if plan.destroys() {
        match req.expect_plan.as_deref() {
            None => {
                return Ok((
                    StatusCode::CONFLICT,
                    Json(serde_json::json!({
                        "applied": false,
                        "code": "plan_confirmation_required",
                        "message": "this apply removes things, so it must confirm the plan it was \
                                    shown; re-check the plan and send its plan_digest",
                        "plan": PlanPreview::from(&plan),
                        "plan_digest": digest,
                    })),
                ))
            }
            Some(confirmed) if confirmed != digest => {
                return Ok((
                    StatusCode::CONFLICT,
                    Json(serde_json::json!({
                        "applied": false,
                        "code": "plan_changed",
                        "message": "the board changed since this plan was shown; nothing was \
                                    applied. Check the new plan and confirm it",
                        "plan": PlanPreview::from(&plan),
                        "plan_digest": digest,
                    })),
                ))
            }
            Some(_) => {}
        }
    }

    let report: ApplyReport = execute(&plan, &existing, &client).await;
    let complete = report.is_complete();
    let status = if complete {
        StatusCode::OK
    } else {
        StatusCode::MULTI_STATUS
    };
    Ok((
        status,
        Json(serde_json::json!({"applied": complete, "report": report})),
    ))
}

/// 422 with every refusal, and — when consent is all that stands in the way — what to grant.
fn refused(refusals: Vec<Refusal>) -> (StatusCode, Json<serde_json::Value>) {
    let listed: Vec<_> = refusals
        .iter()
        .map(|r| serde_json::json!({"refusal": r, "message": r.message()}))
        .collect();
    let mut body = serde_json::json!({
        "applied": false,
        "refusals": listed,
        "message": "the board was refused; nothing was created",
    });
    if let Some(consent) = Consent::of(&refusals) {
        body["consent"] = serde_json::to_value(consent).unwrap_or_default();
    }
    (StatusCode::UNPROCESSABLE_ENTITY, Json(body))
}
