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
        if c.grant.is_empty() {
            None
        } else {
            Some(c)
        }
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
    /// The caller's tier, so this client can consult `auth::policy` for the paths it reaches.
    tier: crate::auth::Tier,
    /// The actor markers, built once and attached to every call.
    ///
    /// This client **bypasses `routes::proxy` entirely**, so nothing in that module applies to it.
    /// Without this, `board/apply` and `instantiate` — the two routes that create whole boards —
    /// would reach the engine with no actor at all, which is the place attribution matters most.
    actor: axum::http::HeaderMap,
}

impl HttpBoardClient {
    pub(crate) fn new(
        state: &AppState,
        project: &Uuid,
        user: &crate::auth::AuthUser,
        tier: crate::auth::Tier,
    ) -> Self {
        let mut actor = axum::http::HeaderMap::new();
        crate::http::actor::set_actor(&mut actor, user, tier);
        Self {
            http: state.http.clone(),
            base: state.engine_base_url(project),
            bearer: format!("Bearer {}", state.cfg.host_secret.expose()),
            tier,
            actor,
        }
    }

    /// A request builder carrying the host bearer and the actor markers, for a path the caller's
    /// tier is allowed to reach.
    ///
    /// **Every** engine call goes through here, and the path is given as SEGMENTS rather than as a
    /// formatted URL so that the same value is both authorised and requested.
    ///
    /// Two defects closed at this one line:
    ///
    /// * `read_board` used to build its own request and arrived unattributed — caught by
    ///   `tiers.rs::the_board_apply_path_carries_the_actor_and_refuses_lower_tiers`, which asserts
    ///   on every request the engine saw rather than on the first.
    /// * This client **bypasses `routes::proxy` entirely**, so `auth::policy` never saw the four
    ///   engine paths it reaches. Default-DENY was advertised for engine access and did not in fact
    ///   cover them. It failed closed — both callers require admin — but "correct because of what
    ///   two other handlers happen to demand" is the kind of accident that survives until it does
    ///   not, so the table is consulted here too.
    fn request(
        &self,
        method: reqwest::Method,
        segments: &[&str],
    ) -> Result<reqwest::RequestBuilder, String> {
        let needed = crate::auth::policy::engine_tier(&method, segments).ok_or_else(|| {
            format!(
                "no policy rule permits {method} /{} through the API",
                segments.join("/")
            )
        })?;
        if self.tier < needed {
            return Err(format!(
                "{method} /{} needs {}, and this caller is {}",
                segments.join("/"),
                needed.as_str(),
                self.tier.as_str()
            ));
        }
        let url = format!("{}/{}", self.base, segments.join("/"));
        Ok(self
            .http
            .request(method, url)
            .header("Authorization", &self.bearer)
            .headers(self.actor.clone()))
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
            .request(reqwest::Method::POST, &["v1", "nodes"])?
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
        let node = id.to_string();
        let resp = self
            .request(reqwest::Method::PATCH, &["v1", "nodes", &node])?
            .json(config)
            .send()
            .await
            .map_err(|e| format!("could not reach the engine: {e}"))?;
        if !resp.status().is_success() {
            return Err(Self::failure(resp).await);
        }
        Ok(())
    }

    async fn add_wire(
        &self,
        from: Uuid,
        to: Uuid,
        wire_type: WireType,
    ) -> Result<Option<String>, String> {
        let resp = self
            .request(reqwest::Method::POST, &["v1", "wires"])?
            .json(&serde_json::json!({"from": from, "to": to, "type": wire_type}))
            .send()
            .await
            .map_err(|e| format!("could not reach the engine: {e}"))?;
        if !resp.status().is_success() {
            return Err(Self::failure(resp).await);
        }
        // The engine's own board-state-flag mechanism (`db::board::add_wire`) returns
        // `{"warning": "..."}` on the same 200 for a wire that was created but deserves the
        // operator's attention. A board applied through this route deserves that warning exactly
        // as much as one wired by hand in the UI — dropping the body here would silently swallow
        // it for every builder-emitted or `wheel.toml`-driven board.
        let body: serde_json::Value = resp.json().await.unwrap_or(serde_json::Value::Null);
        Ok(body
            .get("warning")
            .and_then(|w| w.as_str())
            .map(str::to_string))
    }
}

/// Read the current board into the shape the apply step validates against.
async fn read_board(client: &HttpBoardClient) -> ApiResult<ExistingBoard> {
    let resp = client
        .request(reqwest::Method::GET, &["v1", "board"])
        .map_err(|why| {
            // Not `Box::leak` of the message: a per-call leak on an error path is a leak whatever
            // its size, and the operator wants the detail in the log rather than in the body.
            tracing::warn!(reason = %why, "board read refused by policy");
            ApiError::Forbidden("your role does not permit reading this board")
        })?
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
    crate::auth::AdminScope(scope): crate::auth::AdminScope,
    Json(req): Json<ApplyRequest>,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    // Applying a board creates, patches and wires nodes: it is board structure, which is admin.
    let client = HttpBoardClient::new(&state, &scope.project.id, &scope.user, scope.tier);
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
            let mut body = serde_json::json!({
                "applied": false,
                "refusals": listed,
                "message": "the board was refused; nothing was created",
            });
            if let Some(consent) = Consent::of(&refusals) {
                body["consent"] = serde_json::to_value(consent).unwrap_or_default();
            }
            return Ok((StatusCode::UNPROCESSABLE_ENTITY, Json(body)));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{AuthUser, Tier};
    use axum::routing::post;
    use axum::Router;

    /// A one-route fake engine that answers every `POST /v1/wires` with the given body, so
    /// `HttpBoardClient::add_wire` is exercised against real HTTP bytes rather than asserted by
    /// reading the source.
    async fn fake_engine(body: serde_json::Value) -> String {
        let router = Router::new().route(
            "/v1/wires",
            post(move || {
                let body = body.clone();
                async move { Json(body) }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        tokio::spawn(async move {
            axum::serve(listener, router).await.expect("serve");
        });
        format!("http://{addr}")
    }

    /// `pub(crate)` struct literal, private fields: valid because this test module is a
    /// descendant of the module that defines them. `Tier::Admin` matches what `v1/wires` actually
    /// needs (`auth::policy::engine_tier`), so `request()`'s policy check does not refuse before
    /// the fake engine ever sees the call.
    fn client_against(base: String) -> HttpBoardClient {
        let user = AuthUser::from_redeemed_ticket("test-user".into());
        let mut actor = axum::http::HeaderMap::new();
        crate::http::actor::set_actor(&mut actor, &user, Tier::Admin);
        HttpBoardClient {
            http: reqwest::Client::new(),
            base,
            bearer: "Bearer test".into(),
            tier: Tier::Admin,
            actor,
        }
    }

    /// The engine's own board-state-flag mechanism (`db::board::add_wire`, `board_routes.rs`)
    /// returns `{"warning": "..."}` on a 200 for a wire it created but flagged. This is the one
    /// piece of new logic in the plumbing fix — everything else is `execute`'s job, unit-tested in
    /// `apply.rs` against a fake `BoardClient`.
    #[tokio::test]
    async fn a_warning_in_the_engines_response_reaches_the_caller() {
        let base = fake_engine(serde_json::json!({
            "warning": "exposes notes to unauthenticated input"
        }))
        .await;
        let client = client_against(base);

        let warning = client
            .add_wire(Uuid::new_v4(), Uuid::new_v4(), WireType::Send)
            .await
            .expect("wire created");

        assert_eq!(
            warning.as_deref(),
            Some("exposes notes to unauthenticated input")
        );
    }

    /// The ordinary case — no `warning` field at all — must not be misread as one.
    #[tokio::test]
    async fn no_warning_field_means_none_not_an_error() {
        let base = fake_engine(serde_json::json!({"id": Uuid::new_v4()})).await;
        let client = client_against(base);

        let warning = client
            .add_wire(Uuid::new_v4(), Uuid::new_v4(), WireType::Send)
            .await
            .expect("wire created");

        assert!(warning.is_none(), "{warning:?}");
    }
}
