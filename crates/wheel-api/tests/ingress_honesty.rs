//! A blank 404 from an engine that serves no endpoints is not a typo.
//!
//! The operator opened `/p/<project>/tg`, got a bodiless 404, and read it as a mistyped path. The
//! path was fine: the engine has no `/ingress/*` route at all yet. Those are different facts and
//! the public URL has to say which one it is, because it is the only thing anyone sees.
//!
//! The line is exactly where it can be drawn safely: a 404 with *no body* is the engine having no
//! such route, and only that becomes a 501. A 404 that carries anything is an answer — the
//! engine's `no_such_endpoint`, or an endpoint script's own 404 — and passes through untouched.

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

/// What the mock engine answers an ingress request with.
#[derive(Clone, Copy)]
enum Engine {
    /// No `/ingress/*` route: axum's own empty 404.
    NoIngressRoute,
    /// Serves endpoints, and this path is not one of them.
    NoSuchEndpoint,
    /// An endpoint answered.
    Answers,
}

async fn mock_engine(behaviour: Engine) -> String {
    let app = Router::new()
        .fallback(|State(b): State<Engine>, _req: Request<Body>| async move {
            match b {
                Engine::NoIngressRoute => (StatusCode::NOT_FOUND, Body::empty()).into_response(),
                Engine::NoSuchEndpoint => (
                    StatusCode::NOT_FOUND,
                    axum::Json(
                        json!({"error": {"code": "no_such_endpoint", "message": "no endpoint at /tg"}}),
                    ),
                )
                    .into_response(),
                Engine::Answers => (StatusCode::OK, "hello").into_response(),
            }
        })
        .with_state(behaviour);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

use axum::response::IntoResponse;

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
        master_key: [9u8; 32],
        host_url: "http://host.invalid".into(),
        host_secret: Secret::new("host-secret"),
        engine_port: 7000,
        public_base_url: "https://api.wheel.test".into(),
        max_projects_per_user: 20,
        ingress_rate_per_min: 600,
        ingress_body_limit_bytes: 5 * 1024 * 1024,
        proxy_timeout_secs: 30,
        host_connect_timeout_secs: 3,
    }
}

async fn app(behaviour: Engine) -> Router {
    let path = std::env::temp_dir().join(format!("wheel-ingress-{}.db", uuid::Uuid::new_v4()));
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

async fn call(app: &Router, req: Request<Body>) -> (StatusCode, Vec<u8>) {
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let body = axum::body::to_bytes(res.into_body(), 1 << 20)
        .await
        .unwrap();
    (status, body.to_vec())
}

/// A project of our own, with ingress opened.
async fn project_with_ingress(app: &Router) -> String {
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
    let token = serde_json::from_slice::<serde_json::Value>(&body).unwrap()["token"]
        .as_str()
        .unwrap()
        .to_string();

    let (_, body) = call(
        app,
        Request::builder()
            .method("POST")
            .uri("/v1/projects")
            .header("x-auth-token", &token)
            .header("content-type", "application/json")
            .body(Body::from(json!({"name": "ingress"}).to_string()))
            .unwrap(),
    )
    .await;
    let id = serde_json::from_slice::<serde_json::Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let (status, _) = call(
        app,
        Request::builder()
            .method("PATCH")
            .uri(format!("/v1/projects/{id}"))
            .header("x-auth-token", &token)
            .header("content-type", "application/json")
            .body(Body::from(
                json!({"capabilities": {"http": true}}).to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "could not open ingress");
    id
}

async fn hit(app: &Router, id: &str) -> (StatusCode, Vec<u8>) {
    call(
        app,
        Request::builder()
            .method("GET")
            .uri(format!("/p/{id}/tg"))
            .body(Body::empty())
            .unwrap(),
    )
    .await
}

#[tokio::test]
async fn an_engine_with_no_ingress_route_says_so() {
    let app = app(Engine::NoIngressRoute).await;
    let id = project_with_ingress(&app).await;

    let (status, body) = hit(&app, &id).await;
    assert_eq!(status, StatusCode::NOT_IMPLEMENTED);
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        v["error"]["code"], "ingress_unavailable",
        "the web app keys on this exact string"
    );
    assert!(
        v["error"]["message"]
            .as_str()
            .unwrap()
            .contains("does not serve endpoints"),
        "the message is what the operator reads: {}",
        v["error"]["message"]
    );
}

#[tokio::test]
async fn a_404_the_engine_actually_wrote_passes_through() {
    let app = app(Engine::NoSuchEndpoint).await;
    let id = project_with_ingress(&app).await;

    let (status, body) = hit(&app, &id).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "an answer became a 501");
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["error"]["code"], "no_such_endpoint");
}

#[tokio::test]
async fn a_working_endpoint_is_untouched() {
    let app = app(Engine::Answers).await;
    let id = project_with_ingress(&app).await;

    let (status, body) = hit(&app, &id).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, b"hello");
}
