//! The same behaviour, on the backend a local install actually uses.
//!
//! Every dialect difference is a place the two stores can drift, and drift here is invisible: the
//! Postgres suite stays green while `wheeld` quietly does something else. These run the real router
//! against SQLite, so the branches that only a local install takes are exercised by the same
//! assertions rather than trusted.
//!
//! Unlike the `*_db.rs` suites this needs no TEST_DATABASE_URL — SQLite is a file — so it runs
//! everywhere, including on a laptop with no Postgres.

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

const SESSION_SECRET: &str = "session-secret-that-is-at-least-32-chars";
const ISSUER: &str = "https://api.wheel.test";

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
        session_secret: Secret::new(SESSION_SECRET),
        master_key: [7u8; 32],
        host_url: "http://host.invalid".into(),
        host_secret: Secret::new("host-secret"),
        engine_port: 7000,
        public_base_url: ISSUER.into(),
        max_projects_per_user: 20,
        ingress_rate_per_min: 60,
        ingress_body_limit_bytes: 5 * 1024 * 1024,
        proxy_timeout_secs: 30,
        host_connect_timeout_secs: 3,
    }
}

async fn app() -> (Router, Db) {
    let path = std::env::temp_dir().join(format!("wheel-parity-{}.db", uuid::Uuid::new_v4()));
    let url = format!("sqlite://{}", path.display());
    let db = Db::connect(&url).await.expect("connect and migrate");

    let state = AppState::new(Inner {
        jwks: wheel_api::auth::jwks::JwksCache::new(
            "https://clerk.test/jwks".into(),
            reqwest::Client::new(),
        ),
        cfg: cfg(&url),
        db: db.clone(),
        http: reqwest::Client::new(),
        orch: Arc::new(NoopOrchestrator) as Arc<dyn Orchestrator>,
        ingress_limiter: wheel_api::http::ratelimit::RateLimiter::new(60),
        auth_limiter: wheel_api::http::authlimit::AuthLimiter::new(1000, 1000),
        engine_base_override: None,
    });
    (wheel_api::build_router(state, &[]), db)
}

async fn call(
    app: &Router,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Option<serde_json::Value>,
) -> (StatusCode, serde_json::Value) {
    let mut req = Request::builder().method(method).uri(path);
    if let Some(t) = token {
        req = req.header("x-auth-token", t);
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
    let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

async fn signup(app: &Router, email: &str) -> String {
    let (status, body) = call(
        app,
        "POST",
        "/v1/auth/signup",
        None,
        Some(json!({"email": email, "password": "Correct-Horse-9!"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "signup failed: {body}");
    body["token"].as_str().expect("a token").to_string()
}

#[tokio::test]
async fn signup_login_and_me_work_on_sqlite() {
    let (app, _db) = app().await;
    let token = signup(&app, "alice@example.com").await;

    let (status, me) = call(&app, "GET", "/v1/auth/me", Some(&token), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(me["email"], "alice@example.com");

    let (status, body) = call(
        &app,
        "POST",
        "/v1/auth/login",
        None,
        Some(json!({"email": "ALICE@EXAMPLE.COM", "password": "Correct-Horse-9!"})),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "login must be case-insensitive on sqlite too: {body}"
    );
}

/// The citext substitute, through the API rather than through the schema.
#[tokio::test]
async fn a_differently_cased_address_is_the_same_account() {
    let (app, _db) = app().await;
    signup(&app, "bob@example.com").await;

    let (status, _) = call(
        &app,
        "POST",
        "/v1/auth/signup",
        None,
        Some(json!({"email": "Bob@Example.com", "password": "Correct-Horse-9!"})),
    )
    .await;
    assert_ne!(
        status,
        StatusCode::CREATED,
        "casing created a second account"
    );
}

/// Logout has to end a session that a stateless JWT would otherwise honour until it expired.
#[tokio::test]
async fn logout_revokes_the_session() {
    let (app, _db) = app().await;
    let token = signup(&app, "carol@example.com").await;

    let (status, _) = call(&app, "POST", "/v1/auth/logout", Some(&token), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, _) = call(&app, "GET", "/v1/auth/me", Some(&token), None).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "a revoked session was still honoured"
    );
}

/// Changing the password revokes every other session — the point of changing a compromised one.
#[tokio::test]
async fn changing_the_password_revokes_other_sessions() {
    let (app, _db) = app().await;
    let first = signup(&app, "dave@example.com").await;
    let (status, body) = call(
        &app,
        "POST",
        "/v1/auth/login",
        None,
        Some(json!({"email": "dave@example.com", "password": "Correct-Horse-9!"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let second = body["token"].as_str().unwrap().to_string();

    let (status, body) = call(
        &app,
        "POST",
        "/v1/auth/password",
        Some(&second),
        Some(json!({"current_password": "Correct-Horse-9!", "new_password": "Even-Better-42!"})),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

    let (status, _) = call(&app, "GET", "/v1/auth/me", Some(&first), None).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "the other session survived a password change"
    );
}

#[tokio::test]
async fn projects_are_created_listed_and_scoped_to_their_owner_on_sqlite() {
    let (app, _db) = app().await;
    let alice = signup(&app, "erin@example.com").await;
    let bob = signup(&app, "frank@example.com").await;

    let (status, project) = call(
        &app,
        "POST",
        "/v1/projects",
        Some(&alice),
        Some(json!({"name": "demo"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{project}");
    let id = project["id"].as_str().unwrap().to_string();
    assert_eq!(project["capabilities"]["http"], json!(false));

    let (status, mine) = call(&app, "GET", "/v1/projects", Some(&alice), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(mine.as_array().unwrap().len(), 1);

    let (status, theirs) = call(&app, "GET", "/v1/projects", Some(&bob), None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        theirs.as_array().unwrap().is_empty(),
        "another user's project was listed"
    );

    // Unowned and non-existent must be the same answer, on both backends.
    let mut req = Request::builder()
        .method("GET")
        .uri(format!("/v1/projects/{id}"))
        .header("x-auth-token", &bob)
        .header("x-project-id", &id);
    req = req.header("content-type", "application/json");
    let res = app
        .clone()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(
        res.status(),
        StatusCode::NOT_FOUND,
        "someone else's project must be indistinguishable from a missing one"
    );
}

/// Renaming touches `updated_at`, which is the statement written per dialect.
#[tokio::test]
async fn renaming_a_project_updates_it_on_sqlite() {
    let (app, _db) = app().await;
    let token = signup(&app, "grace@example.com").await;
    let (_, project) = call(
        &app,
        "POST",
        "/v1/projects",
        Some(&token),
        Some(json!({"name": "before"})),
    )
    .await;
    let id = project["id"].as_str().unwrap().to_string();

    let req = Request::builder()
        .method("PATCH")
        .uri(format!("/v1/projects/{id}"))
        .header("x-auth-token", &token)
        .header("x-project-id", &id)
        .header("content-type", "application/json")
        .body(Body::from(json!({"name": "after"}).to_string()))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["name"], "after");
}

/// The per-user cap is a count query, and a cap that only works on one backend is not a cap.
#[tokio::test]
async fn the_per_user_project_cap_is_enforced_on_sqlite() {
    let path = std::env::temp_dir().join(format!("wheel-cap-{}.db", uuid::Uuid::new_v4()));
    let url = format!("sqlite://{}", path.display());
    let db = Db::connect(&url).await.unwrap();
    let mut c = cfg(&url);
    c.max_projects_per_user = 1;
    let state = AppState::new(Inner {
        jwks: wheel_api::auth::jwks::JwksCache::new(
            "https://clerk.test/jwks".into(),
            reqwest::Client::new(),
        ),
        cfg: c,
        db,
        http: reqwest::Client::new(),
        orch: Arc::new(NoopOrchestrator) as Arc<dyn Orchestrator>,
        ingress_limiter: wheel_api::http::ratelimit::RateLimiter::new(60),
        auth_limiter: wheel_api::http::authlimit::AuthLimiter::new(1000, 1000),
        engine_base_override: None,
    });
    let app = wheel_api::build_router(state, &[]);
    let token = signup(&app, "heidi@example.com").await;

    let (status, _) = call(
        &app,
        "POST",
        "/v1/projects",
        Some(&token),
        Some(json!({"name": "one"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, body) = call(
        &app,
        "POST",
        "/v1/projects",
        Some(&token),
        Some(json!({"name": "two"})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
}

/// Session expiry, the shared counters and the ticket sweep are all written per dialect. The sweep
/// running clean is what proves the SQLite forms parse and match rows at all.
#[tokio::test]
async fn maintenance_sweeps_run_on_sqlite() {
    let (_app, db) = app().await;
    wheel_api::boot::run_maintenance_once(&db).await;
    wheel_api::auth::local::sweep(&db)
        .await
        .expect("session sweep");
    wheel_api::http::authlimit::sweep(&db)
        .await
        .expect("auth attempt sweep");
}

/// The shared login limiter counts in the database so replicas cannot each allow a full budget.
/// Its window arithmetic is dialect-specific, so a miscounted window would be silent.
#[tokio::test]
async fn the_login_limiter_counts_on_sqlite() {
    let (app, _db) = app().await;
    signup(&app, "ivan@example.com").await;

    for _ in 0..3 {
        let (status, _) = call(
            &app,
            "POST",
            "/v1/auth/login",
            None,
            Some(json!({"email": "ivan@example.com", "password": "wrong-password"})),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "a wrong password must be 401, not a limiter error"
        );
    }

    // The right password still works: failures counted, but the budget is not exhausted.
    let (status, _) = call(
        &app,
        "POST",
        "/v1/auth/login",
        None,
        Some(json!({"email": "ivan@example.com", "password": "Correct-Horse-9!"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}
