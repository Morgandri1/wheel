// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

//! Who may create an account.
//!
//! With the embedded backend an account's agents run as the daemon's own user, so on a public box
//! open signup hands a stranger code execution on it. Closed, the door is shut to everyone but the
//! owner, who adds people through `POST /v1/auth/users`.

#![cfg(feature = "sqlite")]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use serde_json::{json, Value};
use std::sync::Arc;
use tower::ServiceExt;
use wheel_api::auth::api_token;
use wheel_api::config::{AuthMode, Config, Env, SignupPolicy};
use wheel_api::crypto::Secret;
use wheel_api::db::Db;
use wheel_api::orchestrator::{NoopOrchestrator, Orchestrator};
use wheel_api::state::{AppState, Inner};

const OWNER: &str = "operator@wheeld.invalid";

fn cfg(url: &str, auth_mode: AuthMode, signup: SignupPolicy) -> Config {
    Config {
        env: Env::Prod,
        bind_addr: "127.0.0.1:0".into(),
        database_url: url.into(),
        clerk_jwks_url: "https://clerk.test/jwks".into(),
        clerk_issuer: "https://clerk.test".into(),
        clerk_azp: vec![],
        dev_secret: None,
        auth_mode,
        session_secret: Secret::new("session-secret-that-is-at-least-32-chars"),
        signup,
        master_key: [7u8; 32],
        host_url: "http://host.invalid".into(),
        host_secret: Secret::new("host-secret"),
        engine_port: 7000,
        public_base_url: "https://api.wheel.test".into(),
        max_projects_per_user: 20,
        ingress_rate_per_min: 60,
        ingress_body_limit_bytes: 5 * 1024 * 1024,
        proxy_timeout_secs: 30,
        host_connect_timeout_secs: 3,
        ws_max_bridges_per_project: 16,
        ws_max_lifetime_secs: 3600,
    }
}

async fn store() -> (Db, String) {
    let path = std::env::temp_dir().join(format!("wheel-signup-{}.db", uuid::Uuid::new_v4()));
    let url = format!("sqlite://{}", path.display());
    (Db::connect(&url).await.unwrap(), url)
}

fn router(db: &Db, url: &str, auth_mode: AuthMode, signup: SignupPolicy) -> Router {
    wheel_api::build_router(
        AppState::new(Inner {
            jwks: wheel_api::auth::jwks::JwksCache::new(
                "https://clerk.test/jwks".into(),
                reqwest::Client::new(),
            ),
            cfg: cfg(url, auth_mode, signup),
            db: db.clone(),
            http: reqwest::Client::new(),
            orch: Arc::new(NoopOrchestrator) as Arc<dyn Orchestrator>,
            ingress_limiter: wheel_api::http::ratelimit::RateLimiter::new(60),
            auth_limiter: wheel_api::http::authlimit::AuthLimiter::new(1000, 1000),
            engine_base_override: None,
                membership: wheel_api::membership::MembershipEvents::new(),
            bridges: wheel_api::http::bridges::BridgeCounter::new(),
        }),
        &[],
    )
}

async fn post(app: &Router, path: &str, token: Option<&str>, body: Value) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json");
    if let Some(t) = token {
        req = req.header("x-auth-token", t);
    }
    let res = app
        .clone()
        .oneshot(req.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn get(app: &Router, path: &str, token: &str) -> (StatusCode, Value) {
    let req = Request::builder()
        .uri(path)
        .header("x-auth-token", token)
        .body(Body::empty())
        .unwrap();
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

fn person(email: &str) -> Value {
    json!({"email": email, "password": "Correct-Horse-9!"})
}

async fn owner_token(db: &Db) -> String {
    api_token::bootstrap_owner(db, OWNER, "operator")
        .await
        .unwrap()
        .expect("an empty store gets an owner")
        .token
}

#[tokio::test]
async fn a_closed_signup_is_a_plain_403_and_creates_nothing() {
    let (db, url) = store().await;
    let app = router(&db, &url, AuthMode::Local, SignupPolicy::Closed);
    let (status, body) = post(
        &app,
        "/v1/auth/signup",
        None,
        person("stranger@example.test"),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"]["code"], "forbidden");
    assert!(
        !body.to_string().contains("signup"),
        "the refusal explains itself: {body}"
    );
    assert_eq!(wheel_api::auth::local::count_users(&db).await.unwrap(), 0);
}

#[tokio::test]
async fn an_open_signup_still_creates_an_account() {
    let (db, url) = store().await;
    let app = router(&db, &url, AuthMode::Local, SignupPolicy::Open);
    let (status, _) = post(
        &app,
        "/v1/auth/signup",
        None,
        person("walk-in@example.test"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
}

#[tokio::test]
async fn the_owner_adds_accounts_that_can_then_sign_in() {
    let (db, url) = store().await;
    let owner = owner_token(&db).await;
    let app = router(&db, &url, AuthMode::Local, SignupPolicy::Closed);

    let (status, created) = post(
        &app,
        "/v1/auth/users",
        Some(&owner),
        person("invited@example.test"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["email"], "invited@example.test");
    assert!(created.get("password_hash").is_none(), "{created}");

    let (status, session) =
        post(&app, "/v1/auth/login", None, person("invited@example.test")).await;
    assert_eq!(status, StatusCode::OK);
    let session = session["token"].as_str().unwrap().to_string();

    let (_, me) = get(&app, "/v1/auth/me", &owner).await;
    assert_eq!(me["owner"], true);
    let (_, me) = get(&app, "/v1/auth/me", &session).await;
    assert_eq!(me["owner"], false);

    let (status, _) = post(
        &app,
        "/v1/auth/users",
        Some(&owner),
        person("invited@example.test"),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, _) = post(
        &app,
        "/v1/auth/users",
        Some(&owner),
        json!({"email": "x@example.test", "password": "short"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

/// Anyone who got in some other way — an earlier open signup, or an account the owner made — must
/// not be able to add people: that would reopen a closed door from the inside.
#[tokio::test]
async fn nobody_but_the_owner_adds_accounts() {
    let (db, url) = store().await;
    let owner = owner_token(&db).await;
    let open = router(&db, &url, AuthMode::Local, SignupPolicy::Open);
    let (_, walk_in) = post(
        &open,
        "/v1/auth/signup",
        None,
        person("walk-in@example.test"),
    )
    .await;
    let session = walk_in["token"].as_str().unwrap().to_string();
    let (_, minted) = post(
        &open,
        "/v1/auth/tokens",
        Some(&session),
        json!({"name": "cli"}),
    )
    .await;
    let walk_in_token = minted["token"].as_str().unwrap().to_string();

    let app = router(&db, &url, AuthMode::Local, SignupPolicy::Closed);
    for credential in [session.as_str(), walk_in_token.as_str()] {
        let (status, _) = post(
            &app,
            "/v1/auth/users",
            Some(credential),
            person("friend@example.test"),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }
    let (status, _) = post(&app, "/v1/auth/users", None, person("friend@example.test")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(!owner.is_empty());
}

#[tokio::test]
async fn under_an_identity_provider_there_are_no_local_accounts_to_add() {
    let (db, url) = store().await;
    let owner = owner_token(&db).await;
    let app = router(&db, &url, AuthMode::Jwks, SignupPolicy::Open);
    let (status, _) = post(
        &app,
        "/v1/auth/users",
        Some(&owner),
        person("friend@example.test"),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
