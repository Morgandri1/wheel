// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

//! The API never authenticates by cookie, now or ever.
//!
//! Behind the VPS proxy `/v1` shares an origin with the web app, so a browser attaches the web's
//! session cookie (`__Host-wheel_session`) to every `/v1` request it makes, including ones a hostile
//! page causes. If any cookie could authenticate, that would be ambient authority: CSRF against the
//! whole API. So credentials count only in `x-auth-token` or `Authorization: Bearer`, and a cookie
//! holding a perfectly valid session or token is exactly as good as nothing.

#![cfg(feature = "sqlite")]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use serde_json::{json, Value};
use std::sync::Arc;
use tower::ServiceExt;
use wheel_api::config::{AuthMode, Config, Env, SignupPolicy};
use wheel_api::crypto::Secret;
use wheel_api::db::Db;
use wheel_api::orchestrator::{NoopOrchestrator, Orchestrator};
use wheel_api::state::{AppState, Inner};

fn cfg(url: &str) -> Config {
    Config {
        env: Env::Prod,
        bind_addr: "127.0.0.1:0".into(),
        database_url: url.into(),
        clerk_jwks_url: "https://clerk.test/jwks".into(),
        clerk_issuer: "https://clerk.test".into(),
        clerk_azp: vec![],
        dev_secret: None,
        auth_mode: AuthMode::Local,
        session_secret: Secret::new("session-secret-that-is-at-least-32-chars"),
        signup: SignupPolicy::Open,
        master_key: [7u8; 32],
        host_url: "http://host.invalid".into(),
        host_secret: Secret::new("host-secret"),
        engine_port: 7000,
        public_base_url: "https://wheel.example".into(),
        max_projects_per_user: 20,
        ingress_rate_per_min: 60,
        ingress_body_limit_bytes: 5 * 1024 * 1024,
        proxy_timeout_secs: 30,
        host_connect_timeout_secs: 3,
    }
}

async fn app() -> Router {
    let path = std::env::temp_dir().join(format!("wheel-cookie-{}.db", uuid::Uuid::new_v4()));
    let url = format!("sqlite://{}", path.display());
    let db = Db::connect(&url).await.unwrap();
    wheel_api::build_router(
        AppState::new(Inner {
            jwks: wheel_api::auth::jwks::JwksCache::new(
                "https://clerk.test/jwks".into(),
                reqwest::Client::new(),
            ),
            cfg: cfg(&url),
            db,
            http: reqwest::Client::new(),
            orch: Arc::new(NoopOrchestrator) as Arc<dyn Orchestrator>,
            ingress_limiter: wheel_api::http::ratelimit::RateLimiter::new(60),
            auth_limiter: wheel_api::http::authlimit::AuthLimiter::new(1000, 1000),
            engine_base_override: None,
        }),
        &[],
    )
}

async fn send(
    app: &Router,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut req = Request::builder().method(method).uri(path);
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let req = match body {
        Some(b) => req
            .header("content-type", "application/json")
            .body(Body::from(b.to_string()))
            .unwrap(),
        None => req.body(Body::empty()).unwrap(),
    };
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// A session, an API token and a project, all of them valid.
async fn credentials(app: &Router) -> (String, String, String) {
    let (_, signup) = send(
        app,
        "POST",
        "/v1/auth/signup",
        &[],
        Some(json!({"email": "cookie@example.test", "password": "Correct-Horse-9!"})),
    )
    .await;
    let session = signup["token"].as_str().unwrap().to_string();
    let (_, minted) = send(
        app,
        "POST",
        "/v1/auth/tokens",
        &[("x-auth-token", &session)],
        Some(json!({"name": "cli"})),
    )
    .await;
    let token = minted["token"].as_str().unwrap().to_string();
    let (status, project) = send(
        app,
        "POST",
        "/v1/projects",
        &[("x-auth-token", &session)],
        Some(json!({"name": "cookie"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    (session, token, project["id"].as_str().unwrap().to_string())
}

#[tokio::test]
async fn a_valid_credential_in_a_cookie_authenticates_nothing() {
    let app = app().await;
    let (session, token, project) = credentials(&app).await;

    let jars = [
        format!("__Host-wheel_session={session}"),
        format!("__Host-wheel_session={token}"),
        format!("x-auth-token={session}"),
        format!("authorization=Bearer {token}"),
        format!("wheel_session={session}; __Host-wheel_session={token}; x-auth-token={token}"),
    ];
    let routes = [
        ("GET", "/v1/projects".to_string()),
        ("GET", format!("/v1/projects/{project}")),
        ("GET", "/v1/auth/me".to_string()),
        ("GET", "/v1/auth/tokens".to_string()),
        ("POST", format!("/v1/projects/{project}/ws-ticket")),
    ];
    for jar in &jars {
        for (method, path) in &routes {
            let (status, _) = send(&app, method, path, &[("cookie", jar)], None).await;
            assert_eq!(
                status,
                StatusCode::UNAUTHORIZED,
                "{method} {path} authenticated from the cookie {jar:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_cookie_neither_helps_nor_hurts_the_header_that_does_authenticate() {
    let app = app().await;
    let (session, token, _) = credentials(&app).await;
    let jar = format!("__Host-wheel_session=not-a-session; x-auth-token={token}");
    let (status, _) = send(
        &app,
        "GET",
        "/v1/projects",
        &[("x-auth-token", &session), ("cookie", &jar)],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

/// Logout reads its credential from the same headers. A cookie-carried session must not be
/// revocable by a request the browser was tricked into sending, any more than it is usable.
#[tokio::test]
async fn a_cookie_cannot_log_a_session_out_either() {
    let app = app().await;
    let (session, _, _) = credentials(&app).await;
    let jar = format!("__Host-wheel_session={session}");
    let (status, _) = send(&app, "POST", "/v1/auth/logout", &[("cookie", &jar)], None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = send(
        &app,
        "GET",
        "/v1/projects",
        &[("x-auth-token", &session)],
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a cookie-only logout revoked the session"
    );
}
