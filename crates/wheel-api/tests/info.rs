// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

//! `GET /v1/info` — API-layer capability discovery.
//!
//! The gate, not a tripwire: every id in `routes::info::FEATURES` gets checked against a REAL
//! request against the route it claims exists, the same discipline `wheel-engine`'s own
//! `every_advertised_feature_is_callable_or_configurable` holds its `FEATURES` to. A hardcoded
//! string in a const proves nothing about the running build; a real membership route answering for
//! a real project does.

#![cfg(feature = "sqlite")]

use axum::body::Body;
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
        ws_max_bridges_per_project: 16,
        ws_max_lifetime_secs: 3600,
    }
}

async fn app() -> Router {
    let path = std::env::temp_dir().join(format!("wheel-info-{}.db", uuid::Uuid::new_v4()));
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
        engine_base_override: None,
        membership: wheel_api::membership::MembershipEvents::new(),
        bridges: wheel_api::http::bridges::BridgeCounter::new(),
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

/// A signed-up user and a project they own — admin by construction, per
/// `routes::members`'s own doc comment ("the creator, who is always an admin").
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
            .body(Body::from(json!({"name": "info-test"}).to_string()))
            .unwrap(),
    )
    .await;
    let id = body["id"].as_str().expect("a project id").to_string();
    (token, id)
}

#[tokio::test]
async fn info_is_unauthenticated_and_names_the_running_build() {
    let app = app().await;
    let (status, body) = call(
        &app,
        Request::builder()
            .method("GET")
            .uri("/v1/info")
            .body(Body::empty())
            .unwrap(),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["api_version"], "v1");
    assert!(body["version"].as_str().is_some(), "{body}");
    assert!(
        body["features"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f == "membership"),
        "{body}"
    );
}

/// The gate: `membership` is advertised, so a real membership route must actually answer for a
/// real project, not just exist as a string in `FEATURES`.
#[tokio::test]
async fn the_membership_feature_names_a_route_that_actually_answers() {
    let app = app().await;
    let (token, id) = project(&app).await;

    let (status, body) = call(
        &app,
        Request::builder()
            .method("GET")
            .uri(format!("/v1/projects/{id}/members"))
            .header("x-auth-token", &token)
            .header("x-project-id", &id)
            .body(Body::empty())
            .unwrap(),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["creator"].as_str().is_some(), "{body}");
}

/// A tripwire, not the gate above: keeps `docs/API.md`'s feature table from falling behind the
/// running list, the same discipline `wheel-engine`'s `every_advertised_feature_is_documented` uses
/// for `PROTOCOL.md`.
#[test]
fn every_advertised_feature_is_documented() {
    let api_doc = include_str!("../../../docs/API.md");
    let section = api_doc
        .split("### `GET /v1/info`")
        .nth(1)
        .expect("docs/API.md has a GET /v1/info section");
    for feature in wheel_api::routes::info::FEATURES {
        assert!(
            section.contains(&format!("| `{feature}` |")),
            "{feature} is advertised but docs/API.md does not say what it means"
        );
    }
}
