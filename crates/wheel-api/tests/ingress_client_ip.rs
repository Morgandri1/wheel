// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

//! The client address a public ingress hit carries to the engine.
//!
//! The engine limits ingress per caller and applies `ip_allow` by `x-wheel-client-ip`, so that
//! header is only worth anything if a caller cannot choose it: a trusted proxy's `X-Forwarded-For`
//! names the client, anyone else's is ignored, and a forged `x-wheel-client-ip` never survives.

#![cfg(feature = "sqlite")]

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use axum::Router;
use serde_json::json;
use std::net::SocketAddr;
use std::sync::Arc;
use tower::ServiceExt;
use wheel_api::config::{AuthMode, Config, Env};
use wheel_api::crypto::Secret;
use wheel_api::db::Db;
use wheel_api::http::client_ip::{resolve, TrustedProxies};
use wheel_api::orchestrator::{NoopOrchestrator, Orchestrator};
use wheel_api::state::{AppState, Inner};

/// An engine that answers with the client address it was told, or `none`.
async fn echoing_engine() -> String {
    let app = Router::new().fallback(|req: Request<Body>| async move {
        req.headers()
            .get("x-wheel-client-ip")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("none")
            .to_string()
    });
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
        signup: wheel_api::config::SignupPolicy::Open,
        external: None,
        ws_max_bridges_per_project: 16,
        ws_max_lifetime_secs: 3600,
    }
}

async fn app(trusted: &str) -> Router {
    let path = std::env::temp_dir().join(format!("wheel-cip-{}.db", uuid::Uuid::new_v4()));
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
        engine_base_override: Some(echoing_engine().await),
        external_jwks: None,
        membership: wheel_api::membership::MembershipEvents::new(),
        bridges: wheel_api::http::bridges::BridgeCounter::new(),
    });
    wheel_api::build_router(state, &[]).layer(axum::middleware::from_fn_with_state(
        Arc::new(TrustedProxies::parse(trusted).unwrap()),
        resolve,
    ))
}

async fn body_of(app: &Router, req: Request<Body>) -> (StatusCode, String) {
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

fn json_req(
    method: &str,
    uri: &str,
    token: Option<&str>,
    body: serde_json::Value,
) -> Request<Body> {
    let mut r = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json");
    if let Some(t) = token {
        r = r.header("x-auth-token", t);
    }
    r.body(Body::from(body.to_string())).unwrap()
}

async fn project_with_ingress(app: &Router) -> String {
    let email = format!("{}@example.test", uuid::Uuid::new_v4());
    let (_, signup) = body_of(
        app,
        json_req(
            "POST",
            "/v1/auth/signup",
            None,
            json!({"email": email, "password": "Correct-Horse-9!"}),
        ),
    )
    .await;
    let token = serde_json::from_str::<serde_json::Value>(&signup).unwrap()["token"]
        .as_str()
        .unwrap()
        .to_string();
    let (_, project) = body_of(
        app,
        json_req("POST", "/v1/projects", Some(&token), json!({"name": "cip"})),
    )
    .await;
    let id = serde_json::from_str::<serde_json::Value>(&project).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let (status, _) = body_of(
        app,
        json_req(
            "PATCH",
            &format!("/v1/projects/{id}"),
            Some(&token),
            json!({"capabilities": {"http": true}}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    id
}

fn hit(id: &str, peer: Option<[u8; 4]>, xff: Option<&str>) -> Request<Body> {
    let mut r = Request::builder()
        .uri(format!("/p/{id}/hook"))
        .header("x-wheel-client-ip", "6.6.6.6");
    if let Some(v) = xff {
        r = r.header("x-forwarded-for", v);
    }
    let mut req = r.body(Body::empty()).unwrap();
    if let Some(ip) = peer {
        req.extensions_mut()
            .insert(ConnectInfo(SocketAddr::from((ip, 40000))));
    }
    req
}

#[tokio::test]
async fn behind_a_trusted_proxy_the_engine_is_told_the_real_client() {
    let app = app("127.0.0.1/32").await;
    let id = project_with_ingress(&app).await;
    let (status, seen) = body_of(
        &app,
        hit(&id, Some([127, 0, 0, 1]), Some("6.6.6.6, 198.51.100.7")),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(seen, "198.51.100.7", "the engine was told the wrong caller");
}

#[tokio::test]
async fn an_untrusted_peer_cannot_name_itself_someone_else() {
    let app = app("127.0.0.1/32").await;
    let id = project_with_ingress(&app).await;
    let (_, seen) = body_of(&app, hit(&id, Some([203, 0, 113, 9]), Some("198.51.100.7"))).await;
    assert_eq!(seen, "203.0.113.9");
}

#[tokio::test]
async fn with_no_trusted_proxies_the_peer_is_the_client() {
    let app = app("").await;
    let id = project_with_ingress(&app).await;
    let (_, seen) = body_of(&app, hit(&id, Some([127, 0, 0, 1]), Some("198.51.100.7"))).await;
    assert_eq!(seen, "127.0.0.1");
}

#[tokio::test]
async fn without_a_known_peer_no_address_is_invented_and_a_forged_one_never_survives() {
    let app = app("127.0.0.1/32").await;
    let id = project_with_ingress(&app).await;
    let (_, seen) = body_of(&app, hit(&id, None, Some("198.51.100.7"))).await;
    assert_eq!(seen, "none");
}
