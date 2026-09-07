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
    ExistingBoard, ExistingNode, Plan, WireRef,
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
    pub board: EmittedBoard,
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
}

#[derive(Debug, Serialize)]
pub struct PlanPreview {
    pub create_nodes: Vec<String>,
    pub patch_nodes: Vec<String>,
    /// Structured, not formatted: the confirm step draws these on a canvas.
    pub create_wires: Vec<WireRef>,
}

impl From<&Plan> for PlanPreview {
    fn from(p: &Plan) -> Self {
        Self {
            create_nodes: p.create_nodes.iter().map(|n| n.name.clone()).collect(),
            patch_nodes: p.patch_nodes.iter().map(|n| n.name.clone()).collect(),
            create_wires: p.create_wires.iter().map(WireRef::of_emitted).collect(),
        }
    }
}

/// The engine, over HTTP, with the host bearer the proxy uses.
struct HttpBoardClient {
    http: reqwest::Client,
    base: String,
    bearer: String,
}

impl HttpBoardClient {
    fn new(state: &AppState, project: &Uuid) -> Self {
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
        existing
            .nodes
            .insert(name.to_string(), ExistingNode { id, node_type });
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
    let client = HttpBoardClient::new(&state, &scope.project.id);
    let existing = read_board(&client).await?;

    let policy = ApplyPolicy {
        allow_patch: req.allow_patch,
        allow_wire: req.allow_wire,
    };
    let plan = match validate(&req.board, &existing, policy) {
        Ok(plan) => plan,
        Err(refusals) => {
            let listed: Vec<_> = refusals
                .iter()
                .map(|r| serde_json::json!({"refusal": r, "message": r.message()}))
                .collect();
            return Ok((
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(serde_json::json!({
                    "applied": false,
                    "refusals": listed,
                    "message": "the board was refused; nothing was created",
                })),
            ));
        }
    };

    if req.dry_run {
        return Ok((
            StatusCode::OK,
            Json(serde_json::json!({"applied": false, "plan": PlanPreview::from(&plan)})),
        ));
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
