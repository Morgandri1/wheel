// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

//! `POST /v1/projects/instantiate` — the one-request create+apply sequence.
//!
//! Mirrors `board_apply.rs`'s style (mock engine, local-auth signup) but drives the combined
//! route: the interesting behaviour is what happens to the just-created project when any later
//! step fails, since that project exists nowhere else for the caller to clean up.

#![cfg(feature = "sqlite")]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tower::ServiceExt;
use wheel_api::config::{AuthMode, Config, Env};
use wheel_api::crypto::Secret;
use wheel_api::db::Db;
use wheel_api::models::ProjectStatus;
use wheel_api::orchestrator::{EngineSecrets, NoopOrchestrator, Orchestrator};
use wheel_api::state::{AppState, Inner};

/// How the mock engine behaves when asked to create things.
#[derive(Clone, Copy)]
enum Engine {
    Accepts,
    RefusesWires,
}

async fn mock_engine(behaviour: Engine) -> String {
    use axum::response::IntoResponse;
    use axum::routing::{get, post};
    let app = Router::new()
        .route(
            "/v1/board",
            get(|| async { axum::Json(json!({"nodes": [], "project": {}})) }),
        )
        .route(
            "/v1/nodes",
            post(|axum::Json(_): axum::Json<serde_json::Value>| async move {
                (
                    StatusCode::CREATED,
                    axum::Json(json!({"id": uuid::Uuid::new_v4()})),
                )
                    .into_response()
            }),
        )
        .route(
            "/v1/wires",
            post(move || async move {
                match behaviour {
                    Engine::Accepts => StatusCode::NO_CONTENT.into_response(),
                    Engine::RefusesWires => {
                        (StatusCode::BAD_REQUEST, "wire refused by the engine").into_response()
                    }
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

/// An orchestrator whose `status` after start the test dictates — the only way to exercise
/// "the sandbox never came up" without also making `create_project` itself return an error.
#[derive(Clone)]
struct FakeOrch {
    started_status: ProjectStatus,
    destroyed: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl Orchestrator for FakeOrch {
    async fn host_alive(&self) -> anyhow::Result<()> {
        Ok(())
    }
    async fn provision(&self, _: &uuid::Uuid, _: &EngineSecrets) -> anyhow::Result<()> {
        Ok(())
    }
    async fn start(&self, _: &uuid::Uuid) -> anyhow::Result<()> {
        Ok(())
    }
    async fn stop(&self, _: &uuid::Uuid) -> anyhow::Result<()> {
        Ok(())
    }
    async fn restart(&self, _: &uuid::Uuid) -> anyhow::Result<()> {
        Ok(())
    }
    async fn destroy(&self, _: &uuid::Uuid) -> anyhow::Result<()> {
        self.destroyed.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    async fn status(&self, _: &uuid::Uuid) -> anyhow::Result<ProjectStatus> {
        Ok(self.started_status)
    }
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

async fn app_with(orch: Arc<dyn Orchestrator>, engine_base: String) -> Router {
    let path = std::env::temp_dir().join(format!("wheel-instantiate-{}.db", uuid::Uuid::new_v4()));
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
        orch,
        ingress_limiter: wheel_api::http::ratelimit::RateLimiter::new(600),
        auth_limiter: wheel_api::http::authlimit::AuthLimiter::new(1000, 1000),
        engine_base_override: Some(engine_base),
    });
    wheel_api::build_router(state, &[])
}

async fn app(behaviour: Engine) -> Router {
    let base = mock_engine(behaviour).await;
    app_with(Arc::new(NoopOrchestrator) as Arc<dyn Orchestrator>, base).await
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

async fn signup(app: &Router) -> String {
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
    body["token"].as_str().expect("a session token").to_string()
}

async fn projects_for(app: &Router, token: &str) -> Vec<serde_json::Value> {
    let (_, body) = call(
        app,
        Request::builder()
            .method("GET")
            .uri("/v1/projects")
            .header("x-auth-token", token)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    body.as_array().cloned().unwrap_or_default()
}

async fn instantiate(
    app: &Router,
    token: Option<&str>,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let mut req = Request::builder()
        .method("POST")
        .uri("/v1/projects/instantiate")
        .header("content-type", "application/json");
    if let Some(token) = token {
        req = req.header("x-auth-token", token);
    }
    call(app, req.body(Body::from(body.to_string())).unwrap()).await
}

fn legal_board() -> serde_json::Value {
    json!({
        "name": "from-template",
        "board": {
            "nodes": [
                {"name": "researcher", "type": "agent",
                 "config": {"harness": "claude", "system_prompt": "hi"}},
                {"name": "notes", "type": "ctx", "config": {"markdown": "n"}}
            ],
            "wires": [{"from": "notes", "to": "researcher", "type": "send"}]
        },
        "capabilities": {"http": true}
    })
}

#[tokio::test]
async fn a_legal_template_creates_and_applies_in_one_call() {
    let app = app(Engine::Accepts).await;
    let token = signup(&app).await;

    let (status, body) = instantiate(&app, Some(&token), legal_board()).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["applied"], true);
    assert_eq!(body["project"]["capabilities"]["http"], true, "{body}");
    assert_eq!(body["report"]["created_nodes"].as_array().unwrap().len(), 2);

    // The project the route created is a real, listable project — not a side effect visible only
    // in this response.
    let listed = projects_for(&app, &token).await;
    assert_eq!(listed.len(), 1, "{listed:?}");
}

#[tokio::test]
async fn a_refused_board_creates_no_project_at_all() {
    let app = app(Engine::Accepts).await;
    let token = signup(&app).await;

    let (status, body) = instantiate(
        &app,
        Some(&token),
        json!({
            "name": "bad-template",
            "board": {
                "nodes": [
                    {"name": "a", "type": "agent",
                     "config": {"harness": "claude", "system_prompt": "hi"}},
                    {"name": "v", "type": "vault", "config": {"keys": []}}
                ],
                "wires": [{"from": "a", "to": "v", "type": "write"}]
            }
        }),
    )
    .await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["applied"], false);
    assert!(!body["refusals"].as_array().unwrap().is_empty(), "{body}");

    // Validation ran before anything was created, so there is nothing to clean up either.
    assert_eq!(projects_for(&app, &token).await.len(), 0);
}

#[tokio::test]
async fn a_partial_apply_is_rolled_back_and_the_project_stops_existing() {
    let app = app(Engine::RefusesWires).await;
    let token = signup(&app).await;

    let (status, body) = instantiate(&app, Some(&token), legal_board()).await;
    assert_eq!(status, StatusCode::MULTI_STATUS, "{body}");
    assert_eq!(body["applied"], false);
    assert_eq!(body["rolled_back"], true, "{body}");
    assert!(body["project_id"].is_null(), "{body}");
    let failures = body["report"]["failures"].as_array().unwrap();
    assert_eq!(failures.len(), 1, "{body}");

    // Rolled back means gone, not just reported as failed.
    assert_eq!(projects_for(&app, &token).await.len(), 0);
}

#[tokio::test]
async fn a_sandbox_that_never_comes_up_is_rolled_back_before_any_apply_attempt() {
    let base = mock_engine(Engine::Accepts).await;
    let destroyed = Arc::new(AtomicUsize::new(0));
    let orch = Arc::new(FakeOrch {
        started_status: ProjectStatus::Stopped,
        destroyed: destroyed.clone(),
    }) as Arc<dyn Orchestrator>;
    let app = app_with(orch, base).await;
    let token = signup(&app).await;

    let (status, body) = instantiate(&app, Some(&token), legal_board()).await;
    assert_eq!(status, StatusCode::MULTI_STATUS, "{body}");
    assert_eq!(body["applied"], false);
    assert_eq!(body["rolled_back"], true, "{body}");
    let failures = body["report"]["failures"].as_array().unwrap();
    assert_eq!(failures[0]["step"], "sandbox", "{body}");
    // No nodes were attempted against a sandbox that was never healthy.
    assert!(
        body["report"]["created_nodes"]
            .as_array()
            .unwrap()
            .is_empty(),
        "{body}"
    );
    assert_eq!(destroyed.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn an_unauthenticated_instantiate_is_refused() {
    let app = app(Engine::Accepts).await;
    let (status, _) = instantiate(&app, None, legal_board()).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}
