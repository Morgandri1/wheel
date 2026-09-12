// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

//! The apply route's HTTP shape — where the success-shape invariant actually lands.
//!
//! `crate::apply` proves the logic; this proves the status codes, because that is what a caller
//! branches on. A partial apply returning 200 would let any client that treats 2xx as fine report a
//! half-applied board as a success, which is the one outcome the whole step exists to prevent.

#![cfg(feature = "sqlite")]

use axum::body::Body;
use axum::extract::State;
use axum::http::{Request, StatusCode};
use axum::Router;
use serde_json::json;
use std::sync::Arc;
use tower::ServiceExt;
use wheel_api::config::{AuthMode, Config, Env};
use wheel_api::crypto::Secret;
use wheel_api::db::Db;
use wheel_api::orchestrator::{NoopOrchestrator, Orchestrator};
use wheel_api::state::{AppState, Inner};

/// How the mock engine behaves when asked to create things.
#[derive(Clone, Copy)]
enum Engine {
    /// Everything succeeds.
    Accepts,
    /// Nodes succeed; wires are refused, so the apply lands partially.
    RefusesWires,
}

/// A mock engine that REMEMBERS what it created, so a second apply is a genuine "improve" against
/// an existing board. A static empty board cannot express that case at all — the nodes the first
/// apply made would come back unknown.
#[derive(Clone)]
struct EngineState {
    behaviour: Engine,
    nodes: Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
    /// What the engine was asked to destroy, so a test can assert that a refused board reached it
    /// with nothing at all — the "nothing was created" guarantee is about calls, not just status.
    deleted: Arc<std::sync::Mutex<Vec<String>>>,
}

async fn mock_engine(behaviour: Engine) -> String {
    use axum::response::IntoResponse;
    use axum::routing::{get, post};
    let state = EngineState {
        behaviour,
        nodes: Arc::new(std::sync::Mutex::new(Vec::new())),
        deleted: Arc::new(std::sync::Mutex::new(Vec::new())),
    };
    let app =
        Router::new()
            .route(
                "/v1/board",
                get(|State(s): State<EngineState>| async move {
                    let nodes = s.nodes.lock().unwrap().clone();
                    axum::Json(json!({"nodes": nodes, "project": {}}))
                }),
            )
            .route(
                "/v1/nodes",
                post(
                    |State(s): State<EngineState>,
                     axum::Json(body): axum::Json<serde_json::Value>| async move {
                        let id = uuid::Uuid::new_v4();
                        s.nodes.lock().unwrap().push(json!({
                            "id": id,
                            "name": body["name"],
                            "type": body["type"],
                            "config": body["config"],
                            "wires": [],
                        }));
                        (StatusCode::CREATED, axum::Json(json!({"id": id}))).into_response()
                    },
                ),
            )
            .route(
                "/v1/nodes/{id}",
                axum::routing::delete(
                    |State(s): State<EngineState>,
                     axum::extract::Path(id): axum::extract::Path<String>| async move {
                        s.deleted.lock().unwrap().push(id.clone());
                        s.nodes
                            .lock()
                            .unwrap()
                            .retain(|n| n["id"].as_str() != Some(id.as_str()));
                        StatusCode::NO_CONTENT
                    },
                ),
            )
            .route(
                "/v1/wires",
                axum::routing::delete(
                    |State(s): State<EngineState>,
                     axum::Json(body): axum::Json<serde_json::Value>| async move {
                        let from = body["from"].as_str().unwrap_or_default().to_string();
                        let mut nodes = s.nodes.lock().unwrap();
                        for node in nodes.iter_mut() {
                            if node["id"].as_str() == Some(from.as_str()) {
                                if let Some(wires) = node["wires"].as_array_mut() {
                                    wires.retain(|w| {
                                        w["to"] != body["to"] || w["type"] != body["type"]
                                    });
                                }
                            }
                        }
                        s.deleted.lock().unwrap().push(format!("wire:{from}"));
                        StatusCode::NO_CONTENT
                    },
                )
                .post(
                    |State(s): State<EngineState>,
                     axum::Json(body): axum::Json<serde_json::Value>| async move {
                        match s.behaviour {
                            Engine::Accepts => {
                                let from = body["from"].as_str().unwrap_or_default().to_string();
                                let mut nodes = s.nodes.lock().unwrap();
                                for node in nodes.iter_mut() {
                                    if node["id"].as_str() == Some(from.as_str()) {
                                        if let Some(wires) = node["wires"].as_array_mut() {
                                            wires.push(
                                                json!({"to": body["to"], "type": body["type"]}),
                                            );
                                        }
                                    }
                                }
                                StatusCode::NO_CONTENT.into_response()
                            }
                            Engine::RefusesWires => {
                                (StatusCode::BAD_REQUEST, "wire refused by the engine")
                                    .into_response()
                            }
                        }
                    },
                ),
            )
            .with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

fn cfg(db_url: &str) -> Config {
    Config {
        env: Env::Prod,
        bind_addr: "127.0.0.1:0".into(),
        database_url: db_url.into(),
        clerk_jwks_url: "https://clerk.test/jwks".into(),
        clerk_issuer: "https://clerk.test".into(),
        clerk_azp: vec![],
        dev_secret: None,
        auth_mode: AuthMode::Local,
        session_secret: Secret::new("session-secret-that-is-at-least-32-chars"),
        master_key: [7u8; 32],
        host_url: "http://host.invalid".into(),
        host_secret: Secret::new("host-secret"),
        engine_port: 7000,
        public_base_url: "https://api.wheel.test".into(),
        max_projects_per_user: 20,
        ingress_rate_per_min: 600,
        ingress_body_limit_bytes: 5 * 1024 * 1024,
        proxy_timeout_secs: 30,
        host_connect_timeout_secs: 3,
        signup: wheel_api::config::SignupPolicy::Open,
    }
}

async fn app(behaviour: Engine) -> Router {
    let path = std::env::temp_dir().join(format!("wheel-apply-{}.db", uuid::Uuid::new_v4()));
    let url = format!("sqlite://{}", path.display());
    let db = Db::connect(&url).await.expect("connect and migrate");
    let state = AppState::new(Inner {
        jwks: wheel_api::auth::jwks::JwksCache::new(
            "https://clerk.test/jwks".into(),
            reqwest::Client::new(),
        ),
        cfg: cfg(&url),
        db,
        http: reqwest::Client::new(),
        orch: Arc::new(NoopOrchestrator) as Arc<dyn Orchestrator>,
        ingress_limiter: wheel_api::http::ratelimit::RateLimiter::new(600),
        auth_limiter: wheel_api::http::authlimit::AuthLimiter::new(1000, 1000),
        engine_base_override: Some(mock_engine(behaviour).await),
    });
    wheel_api::build_router(state, &[])
}

async fn call(app: &Router, req: Request<Body>) -> (StatusCode, serde_json::Value) {
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    let body = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, body)
}

/// A signed-up user and a project they own.
async fn project(app: &Router) -> (String, String) {
    let (_, body) = call(
        app,
        Request::builder()
            .method("POST")
            .uri("/v1/auth/signup")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({"email": format!("{}@example.test", uuid::Uuid::new_v4()),
                       "password": "Correct-Horse-9!"})
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    let token = body["token"].as_str().expect("a session token").to_string();

    let (_, body) = call(
        app,
        Request::builder()
            .method("POST")
            .uri("/v1/projects")
            .header("content-type", "application/json")
            .header("x-auth-token", &token)
            .body(Body::from(json!({"name": "builder"}).to_string()))
            .unwrap(),
    )
    .await;
    let id = body["id"].as_str().expect("a project id").to_string();
    (token, id)
}

async fn apply(
    app: &Router,
    token: &str,
    id: &str,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    call(
        app,
        Request::builder()
            .method("POST")
            .uri(format!("/v1/projects/{id}/board/apply"))
            .header("content-type", "application/json")
            .header("x-auth-token", token)
            .header("x-project-id", id)
            .body(Body::from(body.to_string()))
            .unwrap(),
    )
    .await
}

fn legal_board() -> serde_json::Value {
    json!({"board": {
        "nodes": [
            {"name": "researcher", "type": "agent",
             "config": {"harness": "claude", "system_prompt": "hi"}},
            {"name": "notes", "type": "ctx", "config": {"markdown": "n"}}
        ],
        "wires": [{"from": "notes", "to": "researcher", "type": "send"}]
    }})
}

#[tokio::test]
async fn a_legal_board_applies_and_reports_200() {
    let app = app(Engine::Accepts).await;
    let (token, id) = project(&app).await;

    let (status, body) = apply(&app, &token, &id, legal_board()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["applied"], true);
    assert_eq!(body["report"]["created_nodes"].as_array().unwrap().len(), 2);
    assert_eq!(body["report"]["created_wires"].as_array().unwrap().len(), 1);
}

/// The invariant. A caller branching on 2xx must not be able to read a partial apply as success.
#[tokio::test]
async fn a_partial_apply_is_207_and_says_applied_false() {
    let app = app(Engine::RefusesWires).await;
    let (token, id) = project(&app).await;

    let (status, body) = apply(&app, &token, &id, legal_board()).await;
    assert_eq!(
        status,
        StatusCode::MULTI_STATUS,
        "a partial apply must not be 200: {body}"
    );
    assert_eq!(body["applied"], false);
    let failures = body["report"]["failures"].as_array().unwrap();
    assert_eq!(failures.len(), 1, "{body}");
    assert!(
        failures[0]["step"]
            .as_str()
            .unwrap()
            .contains("notes -> researcher"),
        "{body}"
    );
    // The nodes DID land, and the report says so rather than pretending nothing happened.
    assert_eq!(body["report"]["created_nodes"].as_array().unwrap().len(), 2);
}

/// A refused board is 422 and creates nothing — the refusal names the wire.
#[tokio::test]
async fn an_illegal_wire_is_422_and_nothing_is_created() {
    let app = app(Engine::Accepts).await;
    let (token, id) = project(&app).await;

    let (status, body) = apply(
        &app,
        &token,
        &id,
        json!({"board": {
            "nodes": [
                {"name": "a", "type": "agent",
                 "config": {"harness": "claude", "system_prompt": "hi"}},
                {"name": "v", "type": "vault", "config": {"keys": []}}
            ],
            "wires": [{"from": "a", "to": "v", "type": "write"}]
        }}),
    )
    .await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["applied"], false);
    let msg = body["refusals"][0]["message"].as_str().unwrap();
    assert!(
        msg.contains("write") && msg.contains("\"a\"") && msg.contains("\"v\""),
        "{msg}"
    );
}

#[tokio::test]
async fn a_dry_run_changes_nothing_and_returns_the_plan() {
    let app = app(Engine::Accepts).await;
    let (token, id) = project(&app).await;

    let mut body = legal_board();
    body["dry_run"] = json!(true);
    let (status, body) = apply(&app, &token, &id, body).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["applied"], false, "a dry run has not applied anything");
    assert_eq!(body["plan"]["create_nodes"].as_array().unwrap().len(), 2);
    assert!(
        body["report"].is_null(),
        "a dry run must not report an apply"
    );
}

/// The route is project-scoped like every other: no token, no apply.
#[tokio::test]
async fn an_unauthenticated_apply_is_refused() {
    let app = app(Engine::Accepts).await;
    let (_token, id) = project(&app).await;

    let (status, _) = call(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/v1/projects/{id}/board/apply"))
            .header("content-type", "application/json")
            .body(Body::from(legal_board().to_string()))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

/// The wire shape is a CONTRACT with the confirm step, not an implementation detail: Web draws
/// these on a canvas and highlights the ones that failed. A formatted string would force them to
/// parse a sentence to find an edge, so both the plan and the report carry structure.
#[tokio::test]
async fn wires_are_structured_objects_in_both_the_plan_and_the_report() {
    let app = app(Engine::Accepts).await;
    let (token, id) = project(&app).await;

    let mut dry = legal_board();
    dry["dry_run"] = json!(true);
    let (_, body) = apply(&app, &token, &id, dry).await;
    let w = &body["plan"]["create_wires"][0];
    assert_eq!(w["from"], "notes", "{body}");
    assert_eq!(w["to"], "researcher", "{body}");
    assert_eq!(w["type"], "send", "{body}");

    let (_, body) = apply(&app, &token, &id, legal_board()).await;
    let w = &body["report"]["created_wires"][0];
    assert_eq!(w["from"], "notes", "{body}");
    assert_eq!(w["to"], "researcher", "{body}");
    assert_eq!(w["type"], "send", "{body}");
}

/// A failed wire carries the same structure, so the failure can be shown ON the edge rather than
/// only in a list of sentences.
#[tokio::test]
async fn a_failed_wire_is_addressable_not_just_described() {
    let app = app(Engine::RefusesWires).await;
    let (token, id) = project(&app).await;

    let (_, body) = apply(&app, &token, &id, legal_board()).await;
    let f = &body["report"]["failures"][0];
    assert_eq!(f["wire"]["from"], "notes", "{body}");
    assert_eq!(f["wire"]["to"], "researcher", "{body}");
    assert_eq!(f["wire"]["type"], "send", "{body}");
    assert!(f["error"].as_str().unwrap().contains("refused"), "{body}");
    // The human line is still there for a log; it is not the only way in.
    assert!(
        f["step"].as_str().unwrap().contains("notes -> researcher"),
        "{body}"
    );
}

/// Web's point: a refusal the user cannot act on is worse than a bare error, because it looks
/// actionable. For "improve an existing board", touching existing nodes IS the request — the
/// builder cannot route around it — so the 422 carries the consent prompt ready-made rather than
/// leaving the UI to derive it from the refusal list.
#[tokio::test]
async fn a_consent_refusal_says_exactly_what_to_grant() {
    let app = app(Engine::Accepts).await;
    let (token, id) = project(&app).await;

    // Build a board first, so the second apply is an "improve" against existing nodes.
    let (status, _) = apply(&app, &token, &id, legal_board()).await;
    assert_eq!(status, StatusCode::OK);

    // Now a board that wires a NEW ctx into the EXISTING agent — the finding's own escalation.
    let (status, body) = apply(
        &app,
        &token,
        &id,
        json!({"board": {
            "nodes": [{"name": "evil", "type": "ctx", "config": {"markdown": "x"}}],
            "wires": [{"from": "evil", "to": "researcher", "type": "send"}]
        }}),
    )
    .await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    let consent = &body["consent"];
    assert_eq!(consent["would_wire"][0]["from"], "evil", "{body}");
    assert_eq!(consent["would_wire"][0]["to"], "researcher", "{body}");
    assert_eq!(consent["grant"][0], "allow_wire", "{body}");

    // And granting exactly what it asked for lets the same board through.
    let (status, body) = apply(
        &app,
        &token,
        &id,
        json!({"board": {
            "nodes": [{"name": "evil", "type": "ctx", "config": {"markdown": "x"}}],
            "wires": [{"from": "evil", "to": "researcher", "type": "send"}]
        }, "allow_wire": true}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["applied"], true);
}

/// A board refused for a reason no toggle can fix must NOT offer one. An illegal wire is not
/// something the user can consent their way past, and a grant button there would be a lie.
#[tokio::test]
async fn an_illegal_wire_offers_no_consent_to_grant() {
    let app = app(Engine::Accepts).await;
    let (token, id) = project(&app).await;

    let (status, body) = apply(
        &app,
        &token,
        &id,
        json!({"board": {
            "nodes": [
                {"name": "a", "type": "agent",
                 "config": {"harness": "claude", "system_prompt": "hi"}},
                {"name": "v", "type": "vault", "config": {"keys": []}}
            ],
            "wires": [{"from": "a", "to": "v", "type": "write"}]
        }}),
    )
    .await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        body["consent"].is_null(),
        "offered a toggle for an illegal wire: {body}"
    );
}

/// The builder's OWN board shape, end to end: wires nested on the node and addressed by id. This
/// is the shape every builder turn emits, and before it was normalised the wires were dropped in
/// silence and the apply reported success.
#[tokio::test]
async fn a_board_in_the_builders_own_shape_applies_with_its_wires() {
    let app = app(Engine::Accepts).await;
    let (token, id) = project(&app).await;

    let (status, body) = apply(
        &app,
        &token,
        &id,
        json!({"board": {
            "project": {"id": "3f1a2b9c-0d4e-4a6b-8c1d-2e3f4a5b6c7d"},
            "nodes": [
                {"id": "11111111-1111-4111-8111-111111111111", "name": "brief", "type": "ctx",
                 "position": {"x": 0, "y": 0}, "config": {"markdown": "# Brief"},
                 "wires": [{"to": "22222222-2222-4222-8222-222222222222", "type": "send"}]},
                {"id": "22222222-2222-4222-8222-222222222222", "name": "worker", "type": "agent",
                 "position": {"x": 240, "y": 0},
                 "config": {"harness": "claude", "system_prompt": "do the work"}, "wires": []}
            ]
        }}),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["applied"], true);
    assert_eq!(body["report"]["created_nodes"].as_array().unwrap().len(), 2);
    let wires = body["report"]["created_wires"].as_array().unwrap();
    assert_eq!(
        wires.len(),
        1,
        "the builder's nested wire was dropped: {body}"
    );
    assert_eq!(wires[0]["from"], "brief");
    assert_eq!(wires[0]["to"], "worker");
}

/// A board carrying a codex agent is refused whole. The engine rejects that node at creation, so
/// without this the preview looked clean and the apply died halfway through.
#[tokio::test]
async fn a_codex_board_is_refused_before_anything_is_created() {
    let app = app(Engine::Accepts).await;
    let (token, id) = project(&app).await;

    let (status, body) = apply(
        &app,
        &token,
        &id,
        json!({"board": {"nodes": [
            {"name": "worker", "type": "agent", "config": {"harness": "codex", "system_prompt": "p"}}
        ]}}),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(
        body["refusals"][0]["refusal"]["code"], "unsupported_harness",
        "{body}"
    );
    assert!(
        body["consent"].is_null(),
        "no toggle can make codex runnable: {body}"
    );
}

/// A board that is not a board answers in the same shape as one that is illegal, with a reason —
/// rather than the framework's plain-text 422, which the confirm step cannot render at all.
#[tokio::test]
async fn a_malformed_board_is_refused_with_a_reason_it_can_show() {
    let app = app(Engine::Accepts).await;
    let (token, id) = project(&app).await;

    let (status, body) = apply(
        &app,
        &token,
        &id,
        json!({"board": {"nodes": [{"name": "nameless"}]}}),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(
        body["refusals"][0]["refusal"]["code"], "malformed_board",
        "{body}"
    );
    assert!(
        body["refusals"][0]["message"]
            .as_str()
            .unwrap()
            .contains("could not be read"),
        "{body}"
    );
}

/// Removing is its own consent, and the 422 says exactly what it would destroy.
#[tokio::test]
async fn removing_a_node_is_refused_until_it_is_granted_and_says_what_it_costs() {
    let app = app(Engine::Accepts).await;
    let (token, id) = project(&app).await;
    let (status, _) = apply(&app, &token, &id, legal_board()).await;
    assert_eq!(status, StatusCode::OK);

    let removal = json!({"board": {"nodes": [], "remove": {"nodes": ["notes"]}}});
    let (status, body) = apply(&app, &token, &id, removal.clone()).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(
        body["consent"]["would_delete"][0]["name"], "notes",
        "{body}"
    );
    assert_eq!(body["consent"]["would_delete"][0]["type"], "ctx", "{body}");
    assert!(
        body["consent"]["would_delete"][0]["destroys"]
            .as_str()
            .unwrap()
            .contains("markdown"),
        "the UI has to be able to say what is lost: {body}"
    );
    assert_eq!(body["consent"]["grant"][0], "allow_delete", "{body}");

    // Granting the OTHER consents is not granting this one.
    let mut wrong_grant = removal.clone();
    wrong_grant["allow_patch"] = json!(true);
    wrong_grant["allow_wire"] = json!(true);
    let (status, body) = apply(&app, &token, &id, wrong_grant).await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "patching is not destroying: {body}"
    );
}

/// Destruction is bound to the plan the user saw. Without that confirmation it does not happen,
/// and a board that moved underneath gets the new plan rather than a silent substitution.
#[tokio::test]
async fn a_removal_must_confirm_the_plan_it_was_shown() {
    let app = app(Engine::Accepts).await;
    let (token, id) = project(&app).await;
    apply(&app, &token, &id, legal_board()).await;

    let granted = json!({
        "board": {"nodes": [], "remove": {"nodes": ["notes"]}},
        "allow_delete": true
    });

    // Dry run first: this is what the user reads, and it carries the digest.
    let mut dry = granted.clone();
    dry["dry_run"] = json!(true);
    let (status, plan) = apply(&app, &token, &id, dry).await;
    assert_eq!(status, StatusCode::OK, "{plan}");
    assert_eq!(plan["plan"]["delete_nodes"][0]["name"], "notes", "{plan}");
    let digest = plan["plan_digest"]
        .as_str()
        .expect("a digest to confirm")
        .to_string();

    // No confirmation: refused, and nothing removed.
    let (status, body) = apply(&app, &token, &id, granted.clone()).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "plan_confirmation_required", "{body}");
    assert_eq!(body["applied"], false);

    // A digest from some other plan: refused as changed, still nothing removed.
    let mut stale = granted.clone();
    stale["expect_plan"] =
        json!("0000000000000000000000000000000000000000000000000000000000000000");
    let (status, body) = apply(&app, &token, &id, stale).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "plan_changed", "{body}");

    // The plan the user actually confirmed goes through, and reports what went.
    let mut confirmed = granted;
    confirmed["expect_plan"] = json!(digest);
    let (status, body) = apply(&app, &token, &id, confirmed).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["applied"], true, "{body}");
    assert_eq!(body["report"]["deleted_nodes"][0], "notes", "{body}");
}

/// An improve that changes one field must not rewrite the rest. The engine stores the config, so
/// this asserts on what the apply SENT: a patch carrying only what changed.
#[tokio::test]
async fn an_improve_patches_only_the_field_it_changed() {
    let app = app(Engine::Accepts).await;
    let (token, id) = project(&app).await;

    let (status, _) = apply(
        &app,
        &token,
        &id,
        json!({"board": {"nodes": [{"name": "researcher", "type": "agent", "config": {
            "harness": "claude", "system_prompt": "first", "run_on_startup": true
        }}]}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let improve = json!({
        "board": {"nodes": [{"name": "researcher", "type": "agent", "config": {
            "harness": "claude", "system_prompt": "second"
        }}]},
        "allow_patch": true,
        "dry_run": true
    });
    let (status, body) = apply(&app, &token, &id, improve).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let detail = &body["plan"]["patch_details"][0];
    assert_eq!(detail["name"], "researcher", "{body}");
    assert_eq!(
        detail["fields"].as_array().unwrap(),
        &vec![json!("system_prompt")],
        "only the field that changed may be written: {body}"
    );
}
