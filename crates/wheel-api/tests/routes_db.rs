//! The API's proxy and ingress routes, against a mock engine.
//!
//! `projects_db.rs` covers who may reach a project. This file covers what happens to the request
//! once they may: which headers survive the hop, which are invented, and which are refused.
//!
//! Header hygiene is the substance here. A proxy that forwards naively is a confused deputy — it
//! holds credentials the caller does not, so anything the caller can smuggle through it inherits
//! that authority.

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, Request, StatusCode};
use axum::Router;
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use serde_json::json;
use std::sync::{Arc, Mutex};
use tower::ServiceExt;
use wheel_api::config::{Config, Env};
use wheel_api::crypto::Secret;
use wheel_api::orchestrator::{NoopOrchestrator, Orchestrator};
use wheel_api::state::{AppState, Inner};

const ISSUER: &str = "https://dev.wheel.local";
const DEV_SECRET: &str = "integration-test-secret";
const USER_JWT_MARKER: &str = "user-token-must-not-be-forwarded";

// ---------------------------------------------------------------- mock engine

#[derive(Clone, Default)]
struct Seen {
    headers: Arc<Mutex<Option<HeaderMap>>>,
    path: Arc<Mutex<Option<String>>>,
    body: Arc<Mutex<Option<Vec<u8>>>>,
}

async fn mock_engine() -> (String, Seen) {
    let seen = Seen::default();
    let app = Router::new()
        .fallback(|State(s): State<Seen>, req: Request<Body>| async move {
            *s.path.lock().unwrap() = Some(req.uri().path().to_string());
            *s.headers.lock().unwrap() = Some(req.headers().clone());
            let bytes = axum::body::to_bytes(req.into_body(), 1 << 24)
                .await
                .unwrap();
            *s.body.lock().unwrap() = Some(bytes.to_vec());
            axum::Json(json!({"nodes": []}))
        })
        .with_state(seen.clone());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), seen)
}

// ---------------------------------------------------------------- harness

fn cfg(db_url: &str) -> Config {
    Config {
        env: Env::Dev,
        bind_addr: "127.0.0.1:0".into(),
        database_url: db_url.into(),
        clerk_jwks_url: "http://unused.invalid/jwks".into(),
        clerk_issuer: ISSUER.into(),
        clerk_azp: vec![],
        dev_secret: Some(DEV_SECRET.into()),
        auth_mode: wheel_api::config::AuthMode::Jwks,
        session_secret: wheel_api::crypto::Secret::new("test-session-secret-at-least-32-chars"),
        master_key: [3u8; 32],
        host_url: "http://host.invalid".into(),
        host_secret: Secret::new("host-secret"),
        engine_port: 7000,
        public_base_url: "http://localhost".into(),
        max_projects_per_user: 20,
        ingress_rate_per_min: 60,
        ingress_body_limit_bytes: 1024,
        proxy_timeout_secs: 30,
        host_connect_timeout_secs: 3,
    }
}

fn user() -> String {
    format!("user_{}", uuid::Uuid::new_v4())
}

fn token(sub: &str) -> String {
    #[derive(serde::Serialize)]
    struct C<'a> {
        sub: &'a str,
        iss: &'a str,
        exp: i64,
        nbf: i64,
    }
    let now = chrono::Utc::now().timestamp();
    jsonwebtoken::encode(
        &Header::new(Algorithm::HS256),
        &C {
            sub,
            iss: ISSUER,
            exp: now + 3600,
            nbf: now - 60,
        },
        &EncodingKey::from_secret(DEV_SECRET.as_bytes()),
    )
    .unwrap()
}

async fn app(engine: String) -> Option<Router> {
    let url = match std::env::var("TEST_DATABASE_URL") {
        Ok(u) => u,
        // Gated on a promised database, not on being in CI: keying this off `CI` asserts every CI
        // job has Postgres, which reddened main once already.
        Err(_) if std::env::var("WHEEL_CI_HAS_DB").as_deref() == Ok("1") => {
            panic!("WHEEL_CI_HAS_DB=1 but TEST_DATABASE_URL is unset")
        }
        Err(_) => {
            eprintln!("skipping {}: TEST_DATABASE_URL not set", module_path!());
            return None;
        }
    };
    let db = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect(&url)
        .await
        .expect("connect to TEST_DATABASE_URL");
    sqlx::migrate!("./migrations").run(&db).await.unwrap();

    let state = AppState::new(Inner {
        jwks: wheel_api::auth::jwks::JwksCache::new(
            "http://unused.invalid/jwks".into(),
            reqwest::Client::new(),
        ),
        cfg: cfg(&url),
        db,
        http: reqwest::Client::new(),
        orch: Arc::new(NoopOrchestrator) as Arc<dyn Orchestrator>,
        ingress_limiter: wheel_api::http::ratelimit::RateLimiter::new(60),
        auth_limiter: wheel_api::http::authlimit::AuthLimiter::new(1000, 1000),
        engine_base_override: Some(engine),
    });
    Some(wheel_api::build_router(state, &[]))
}

async fn send(app: &Router, req: Request<Body>) -> (StatusCode, Vec<u8>) {
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 24)
        .await
        .unwrap();
    (status, bytes.to_vec())
}

async fn make_project(app: &Router, tok: &str) -> String {
    let req = Request::builder()
        .method("POST")
        .uri("/v1/projects")
        .header("x-auth-token", tok)
        .header("content-type", "application/json")
        .body(Body::from(json!({"name": "routes"}).to_string()))
        .unwrap();
    let (status, body) = send(app, req).await;
    assert_eq!(status, StatusCode::CREATED);
    serde_json::from_slice::<serde_json::Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string()
}

async fn open_ingress(app: &Router, tok: &str, id: &str) {
    let req = Request::builder()
        .method("PATCH")
        .uri(format!("/v1/projects/{id}"))
        .header("x-auth-token", tok)
        .header("content-type", "application/json")
        .body(Body::from(
            json!({"capabilities": {"http": true}}).to_string(),
        ))
        .unwrap();
    assert_eq!(send(app, req).await.0, StatusCode::OK);
}

// ---------------------------------------------------------------- proxy

#[tokio::test]
async fn proxy_strips_the_users_credentials_before_the_hop() {
    let (engine, seen) = mock_engine().await;
    let Some(app) = app(engine).await else { return };
    let tok = token(&user());
    let id = make_project(&app, &tok).await;

    let req = Request::builder()
        .method("GET")
        .uri(format!("/v1/projects/{id}/engine/v1/board"))
        .header("x-auth-token", &tok)
        .header("authorization", format!("Bearer {USER_JWT_MARKER}"))
        .header("x-custom", "kept")
        .body(Body::empty())
        .unwrap();
    let (status, _) = send(&app, req).await;
    assert_eq!(status, StatusCode::OK);

    let h = seen
        .headers
        .lock()
        .unwrap()
        .clone()
        .expect("engine saw no request");
    assert!(
        h.get("x-auth-token").is_none(),
        "the session JWT reached the engine"
    );
    let auth = h
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        !auth.contains(USER_JWT_MARKER),
        "the caller's own Authorization header was relayed downstream"
    );
    assert_eq!(
        h.get("x-custom").and_then(|v| v.to_str().ok()),
        Some("kept")
    );
}

#[tokio::test]
async fn proxy_drops_headers_the_caller_nominates_via_connection() {
    // `Connection: x-secret` means "this header is hop-by-hop". Honouring the list is what stops a
    // caller from nominating arbitrary headers for removal, or smuggling framing directives.
    let (engine, seen) = mock_engine().await;
    let Some(app) = app(engine).await else { return };
    let tok = token(&user());
    let id = make_project(&app, &tok).await;

    let req = Request::builder()
        .method("GET")
        .uri(format!("/v1/projects/{id}/engine/v1/board"))
        .header("x-auth-token", &tok)
        .header("connection", "x-nominated")
        .header("x-nominated", "should-not-survive")
        .body(Body::empty())
        .unwrap();
    assert_eq!(send(&app, req).await.0, StatusCode::OK);

    let h = seen.headers.lock().unwrap().clone().unwrap();
    assert!(h.get("x-nominated").is_none());
    assert!(h.get("connection").is_none());
}

#[tokio::test]
async fn proxy_forwards_the_body_and_the_query_string() {
    let (engine, seen) = mock_engine().await;
    let Some(app) = app(engine).await else { return };
    let tok = token(&user());
    let id = make_project(&app, &tok).await;

    let req = Request::builder()
        .method("POST")
        .uri(format!("/v1/projects/{id}/engine/v1/nodes?dry_run=1"))
        .header("x-auth-token", &tok)
        .header("content-type", "application/json")
        .body(Body::from(r#"{"type":"ctx"}"#))
        .unwrap();
    assert_eq!(send(&app, req).await.0, StatusCode::OK);

    assert_eq!(seen.path.lock().unwrap().clone().unwrap(), "/v1/nodes");
    assert_eq!(
        seen.body.lock().unwrap().clone().unwrap(),
        br#"{"type":"ctx"}"#.to_vec()
    );
}

#[tokio::test]
async fn proxy_refuses_traversal_before_the_hop() {
    let (engine, seen) = mock_engine().await;
    let Some(app) = app(engine).await else { return };
    let tok = token(&user());
    let id = make_project(&app, &tok).await;

    let req = Request::builder()
        .method("GET")
        .uri(format!("/v1/projects/{id}/engine/v1/../../etc/passwd"))
        .header("x-auth-token", &tok)
        .body(Body::empty())
        .unwrap();
    let (status, _) = send(&app, req).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        seen.path.lock().unwrap().is_none(),
        "a traversal attempt must never reach the engine"
    );
}

// ---------------------------------------------------------------- ingress

#[tokio::test]
async fn ingress_is_403_until_opted_in_then_reaches_the_engine() {
    let (engine, seen) = mock_engine().await;
    let Some(app) = app(engine).await else { return };
    let tok = token(&user());
    let id = make_project(&app, &tok).await;

    let closed = Request::builder()
        .method("GET")
        .uri(format!("/p/{id}/hook"))
        .body(Body::empty())
        .unwrap();
    assert_eq!(send(&app, closed).await.0, StatusCode::FORBIDDEN);
    assert!(seen.path.lock().unwrap().is_none());

    open_ingress(&app, &tok, &id).await;

    let open = Request::builder()
        .method("GET")
        .uri(format!("/p/{id}/hook"))
        .body(Body::empty())
        .unwrap();
    assert_eq!(send(&app, open).await.0, StatusCode::OK);
    assert_eq!(seen.path.lock().unwrap().clone().unwrap(), "/ingress/hook");
}

#[tokio::test]
async fn ingress_cannot_forge_the_wheel_header_namespace() {
    // `x-wheel-ingress` is a trust marker the engine reads. A public caller setting it themselves
    // must not be able to make a request look like it came from us.
    let (engine, seen) = mock_engine().await;
    let Some(app) = app(engine).await else { return };
    let tok = token(&user());
    let id = make_project(&app, &tok).await;
    open_ingress(&app, &tok, &id).await;

    let req = Request::builder()
        .method("GET")
        .uri(format!("/p/{id}/hook"))
        .header("x-wheel-ingress", "forged")
        .header("x-wheel-anything", "forged")
        .body(Body::empty())
        .unwrap();
    assert_eq!(send(&app, req).await.0, StatusCode::OK);

    let h = seen.headers.lock().unwrap().clone().unwrap();
    assert_eq!(
        h.get("x-wheel-ingress").and_then(|v| v.to_str().ok()),
        Some("1"),
        "our own marker should be the one that arrives"
    );
    assert!(
        h.get("x-wheel-anything").is_none(),
        "a forged x-wheel-* header survived"
    );
}

#[tokio::test]
async fn ingress_for_an_unknown_project_is_404_not_403() {
    // 404 for unknown, 403 for known-but-disabled: the pair must not collapse into an oracle that
    // reveals which project ids exist.
    let (engine, _) = mock_engine().await;
    let Some(app) = app(engine).await else { return };
    let req = Request::builder()
        .method("GET")
        .uri(format!("/p/{}/hook", uuid::Uuid::new_v4()))
        .body(Body::empty())
        .unwrap();
    assert_eq!(send(&app, req).await.0, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn ingress_enforces_the_body_cap() {
    let (engine, _) = mock_engine().await;
    let Some(app) = app(engine).await else { return };
    let tok = token(&user());
    let id = make_project(&app, &tok).await;
    open_ingress(&app, &tok, &id).await;

    // The harness sets the cap to 1 KiB.
    let req = Request::builder()
        .method("POST")
        .uri(format!("/p/{id}/hook"))
        .body(Body::from(vec![b'x'; 4096]))
        .unwrap();
    assert_eq!(send(&app, req).await.0, StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn ingress_rate_limit_refuses_once_the_budget_is_spent() {
    let (engine, _) = mock_engine().await;
    let Some(app) = app(engine).await else { return };
    let tok = token(&user());
    let id = make_project(&app, &tok).await;
    open_ingress(&app, &tok, &id).await;

    let mut saw_429 = false;
    for _ in 0..70 {
        let req = Request::builder()
            .method("GET")
            .uri(format!("/p/{id}/hook"))
            .body(Body::empty())
            .unwrap();
        if send(&app, req).await.0 == StatusCode::TOO_MANY_REQUESTS {
            saw_429 = true;
            break;
        }
    }
    assert!(saw_429, "the 60/min ingress budget was never enforced");
}
