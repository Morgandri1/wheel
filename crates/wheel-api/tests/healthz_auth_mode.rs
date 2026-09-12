// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

//! API-healthz-publishes-auth-mode.
//!
//! `AUTH_MODE` on the API and `NEXT_PUBLIC_AUTH_MODE` in the web build must agree. When they do not,
//! the user gets a login widget whose token we reject, or a form talking to a verifier that is not
//! running — and neither side's tests can see it, because each half is correct on its own. It is a
//! deploy-time disagreement between two correct halves, so it needs a fact both halves can read.
//!
//! `/healthz` publishes that fact. Web asserts against it in their smoke path, so a mismatch is a
//! red gate rather than a support ticket.
//!
//! The second half of this file is the part worth attacking, and Web asked ADVERSARY to rule on it:
//! the route is unauthenticated. Publishing the *mode* gives away nothing that `POST /v1/auth/login`
//! answering 401 rather than 404 does not already reveal. That argument covers the mode and stops
//! there, so these tests hold the response to the mode and nothing further — no issuer, no JWKS URL,
//! no key material.

// Exercises the SQLite backend, so it exists only in a build that has one.
#![cfg(feature = "sqlite")]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use std::sync::Arc;
use tower::ServiceExt;
use wheel_api::config::{AuthMode, Config, Env};
use wheel_api::crypto::Secret;
use wheel_api::db::Db;
use wheel_api::orchestrator::{NoopOrchestrator, Orchestrator};
use wheel_api::state::{AppState, Inner};

/// Distinctive so a leak is unmistakable in an assertion failure rather than plausible.
const ISSUER: &str = "https://clerk-issuer-should-not-be-published.test";
const JWKS_URL: &str = "https://clerk-jwks-should-not-be-published.test/jwks";
const SESSION_SECRET: &str = "session-secret-that-is-at-least-32-chars";
const HOST_SECRET: &str = "host-secret-must-never-be-published";

fn cfg(db_url: &str, mode: AuthMode) -> Config {
    Config {
        env: Env::Prod,
        bind_addr: "127.0.0.1:0".into(),
        database_url: db_url.into(),
        clerk_jwks_url: JWKS_URL.into(),
        clerk_issuer: ISSUER.into(),
        clerk_azp: vec![],
        dev_secret: None,
        auth_mode: mode,
        session_secret: Secret::new(SESSION_SECRET),
        master_key: [7u8; 32],
        host_url: "http://host.invalid".into(),
        host_secret: Secret::new(HOST_SECRET),
        engine_port: 7000,
        public_base_url: "https://api.wheel.test".into(),
        max_projects_per_user: 20,
        ingress_rate_per_min: 60,
        ingress_body_limit_bytes: 5 * 1024 * 1024,
        proxy_timeout_secs: 30,
        host_connect_timeout_secs: 3,
        signup: wheel_api::config::SignupPolicy::Open,
        external: None,
        ws_max_bridges_per_project: 16,
        ws_max_lifetime_secs: 3600,
    }
}

async fn app(mode: AuthMode) -> Router {
    let path = std::env::temp_dir().join(format!("wheel-healthz-{}.db", uuid::Uuid::new_v4()));
    let url = format!("sqlite://{}", path.display());
    let db = Db::connect(&url).await.expect("connect and migrate");
    let state = AppState::new(Inner {
        jwks: wheel_api::auth::jwks::JwksCache::new(JWKS_URL.into(), reqwest::Client::new()),
        cfg: cfg(&url, mode),
        db,
        http: reqwest::Client::new(),
        orch: Arc::new(NoopOrchestrator) as Arc<dyn Orchestrator>,
        ingress_limiter: wheel_api::http::ratelimit::RateLimiter::new(60),
        auth_limiter: wheel_api::http::authlimit::AuthLimiter::new(1000, 1000),
        engine_base_override: None,
        external_jwks: None,
        membership: wheel_api::membership::MembershipEvents::new(),
        bridges: wheel_api::http::bridges::BridgeCounter::new(),
    });
    wheel_api::build_router(state, &[])
}

async fn healthz(mode: AuthMode) -> (StatusCode, serde_json::Value, String) {
    let app = app(mode).await;
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/healthz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .unwrap();
    let raw = String::from_utf8(bytes.to_vec()).expect("healthz answers utf-8");
    let v: serde_json::Value = serde_json::from_str(&raw).expect("healthz answers json");
    (status, v, raw)
}

#[tokio::test]
async fn healthz_reports_the_mode_it_is_actually_running() {
    let (status, v, _) = healthz(AuthMode::Local).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["status"], "ok", "liveness must not regress");
    assert_eq!(v["auth_mode"], "local");

    let (status, v, _) = healthz(AuthMode::Jwks).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["auth_mode"], "jwks");
}

/// The two spellings a client compares against are the two `AUTH_MODE` accepts. If they ever drift
/// apart, the interlock silently stops matching and reports agreement that is not there.
#[test]
fn the_published_spelling_is_the_one_the_env_var_accepts() {
    assert_eq!(AuthMode::Local.as_str(), "local");
    assert_eq!(AuthMode::Jwks.as_str(), "jwks");
}

/// The route is unauthenticated. The argument for publishing the mode is that the login route's
/// existence already reveals it — that covers the mode exactly, and nothing else here is covered by
/// anything.
#[tokio::test]
async fn healthz_publishes_the_mode_and_nothing_else_about_auth() {
    for mode in [AuthMode::Local, AuthMode::Jwks] {
        let (_, v, raw) = healthz(mode).await;

        for leaked in [ISSUER, JWKS_URL, SESSION_SECRET, HOST_SECRET] {
            assert!(
                !raw.contains(leaked),
                "healthz published {leaked:?} in {mode:?} mode: {raw}"
            );
        }
        assert!(
            !raw.contains("clerk") && !raw.contains("jwks.test"),
            "healthz named the provider rather than the mode: {raw}"
        );

        let obj = v.as_object().expect("an object");
        let mut keys: Vec<_> = obj.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            ["auth_mode", "status"],
            "an unauthenticated probe grew a field; every one is public forever: {raw}"
        );
    }
}
