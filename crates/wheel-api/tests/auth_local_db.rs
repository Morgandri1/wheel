//! Local authentication end to end.
//!
//! Weighted toward the properties that are invisible when they break: that failures are
//! indistinguishable, that logout actually ends a session, and that a token minted for one mode is
//! worthless in the other.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use serde_json::json;
use std::sync::Arc;
use tower::ServiceExt;
use wheel_api::config::{AuthMode, Config, Env};
use wheel_api::crypto::Secret;
use wheel_api::orchestrator::{NoopOrchestrator, Orchestrator};
use wheel_api::state::{AppState, Inner};

const SESSION_SECRET: &str = "session-secret-that-is-at-least-32-chars";
const ISSUER: &str = "https://api.wheel.test";

fn cfg(db_url: &str, mode: AuthMode) -> Config {
    Config {
        env: Env::Prod,
        bind_addr: "127.0.0.1:0".into(),
        database_url: db_url.into(),
        clerk_jwks_url: "https://clerk.test/jwks".into(),
        clerk_issuer: "https://clerk.test".into(),
        clerk_azp: vec![],
        dev_secret: None,
        auth_mode: mode,
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

async fn app_with(mode: AuthMode) -> Option<(Router, sqlx::PgPool)> {
    let url = match std::env::var("TEST_DATABASE_URL") {
        Ok(u) => u,
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
        .expect("connect");
    sqlx::migrate!("./migrations").run(&db).await.unwrap();

    let state = AppState::new(Inner {
        jwks: wheel_api::auth::jwks::JwksCache::new(
            "https://clerk.test/jwks".into(),
            reqwest::Client::new(),
        ),
        cfg: cfg(&url, mode),
        db: db.clone(),
        http: reqwest::Client::new(),
        orch: Arc::new(NoopOrchestrator) as Arc<dyn Orchestrator>,
        ingress_limiter: wheel_api::http::ratelimit::RateLimiter::new(60),
        auth_limiter: wheel_api::http::authlimit::AuthLimiter::new(1000, 1000),
        engine_base_override: None,
    });
    Some((wheel_api::build_router(state, &[]), db))
}

async fn app() -> Option<(Router, sqlx::PgPool)> {
    app_with(AuthMode::Local).await
}

async fn call(
    app: &Router,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Option<serde_json::Value>,
) -> (StatusCode, serde_json::Value) {
    let mut b = Request::builder().method(method).uri(path);
    if let Some(t) = token {
        b = b.header("x-auth-token", t);
    }
    let req = match body {
        Some(v) => b
            .header("content-type", "application/json")
            .body(Body::from(v.to_string()))
            .unwrap(),
        None => b.body(Body::empty()).unwrap(),
    };
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
    )
}

fn email() -> String {
    format!("user-{}@example.com", uuid::Uuid::new_v4())
}

async fn signup(app: &Router, email: &str, password: &str) -> (StatusCode, serde_json::Value) {
    call(
        app,
        "POST",
        "/v1/auth/signup",
        None,
        Some(json!({"email": email, "password": password})),
    )
    .await
}

async fn login(app: &Router, email: &str, password: &str) -> (StatusCode, serde_json::Value) {
    call(
        app,
        "POST",
        "/v1/auth/login",
        None,
        Some(json!({"email": email, "password": password})),
    )
    .await
}

// ---------------------------------------------------------------- the happy path

#[tokio::test]
async fn signup_then_use_the_session() {
    let Some((app, _db)) = app().await else {
        return;
    };
    let e = email();

    let (status, body) = signup(&app, &e, "a-long-enough-password").await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let token = body["token"].as_str().unwrap().to_string();
    assert_eq!(body["user"]["email"], e);
    assert!(
        body["user"].get("password_hash").is_none(),
        "password hash leaked to the client"
    );

    let (status, me) = call(&app, "GET", "/v1/auth/me", Some(&token), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(me["email"], e);

    // The session works on the rest of the API, not just on /auth — that is the point of it ending
    // at the same VerifiedUser the ownership extractor consumes.
    let (status, _) = call(&app, "GET", "/v1/projects", Some(&token), None).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn login_issues_a_working_session() {
    let Some((app, _db)) = app().await else {
        return;
    };
    let e = email();
    signup(&app, &e, "a-long-enough-password").await;

    let (status, body) = login(&app, &e, "a-long-enough-password").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let token = body["token"].as_str().unwrap();
    assert_eq!(
        call(&app, "GET", "/v1/auth/me", Some(token), None).await.0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn email_case_and_padding_do_not_create_a_second_account() {
    // Without citext and normalisation, Alice@ and alice@ are two users who cannot see each
    // other's projects, and the second signup silently succeeds.
    let Some((app, _db)) = app().await else {
        return;
    };
    let e = email();
    assert_eq!(
        signup(&app, &e, "a-long-enough-password").await.0,
        StatusCode::CREATED
    );

    let (status, _) = signup(
        &app,
        &format!("  {}  ", e.to_uppercase()),
        "a-long-enough-password",
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "a case variant created a second account"
    );

    // And it logs in as the original user.
    let (status, body) = login(&app, &e.to_uppercase(), "a-long-enough-password").await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

// ---------------------------------------------------------------- failure is uniform

#[tokio::test]
async fn every_login_failure_looks_the_same() {
    // An unknown email and a wrong password must be indistinguishable, or the endpoint tells an
    // attacker which addresses are registered.
    let Some((app, _db)) = app().await else {
        return;
    };
    let e = email();
    signup(&app, &e, "a-long-enough-password").await;

    let (wrong_pw, body_a) = login(&app, &e, "the-wrong-password").await;
    let (unknown, body_b) = login(&app, &email(), "the-wrong-password").await;
    let (malformed, body_c) = login(&app, "not-an-email", "the-wrong-password").await;

    assert_eq!(wrong_pw, StatusCode::UNAUTHORIZED);
    assert_eq!(unknown, StatusCode::UNAUTHORIZED);
    assert_eq!(malformed, StatusCode::UNAUTHORIZED);
    assert_eq!(body_a, body_b, "wrong password and unknown account differ");
    assert_eq!(body_b, body_c, "malformed input is distinguishable");
}

#[tokio::test]
async fn weak_and_malformed_input_is_refused() {
    let Some((app, _db)) = app().await else {
        return;
    };
    assert_eq!(
        signup(&app, &email(), "short").await.0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        signup(&app, "not-an-email", "a-long-enough-password")
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        signup(&app, "", "a-long-enough-password").await.0,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn unauthenticated_and_garbage_tokens_are_refused() {
    let Some((app, _db)) = app().await else {
        return;
    };
    assert_eq!(
        call(&app, "GET", "/v1/auth/me", None, None).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(&app, "GET", "/v1/auth/me", Some("garbage"), None)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(&app, "GET", "/v1/auth/me", Some("a.b.c"), None)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
}

// ---------------------------------------------------------------- revocation

#[tokio::test]
async fn logout_actually_ends_the_session() {
    // The property that a stateless JWT cannot provide on its own: after logout the token must
    // stop working, not merely stop being sent.
    let Some((app, _db)) = app().await else {
        return;
    };
    let e = email();
    let (_, body) = signup(&app, &e, "a-long-enough-password").await;
    let token = body["token"].as_str().unwrap().to_string();

    assert_eq!(
        call(&app, "GET", "/v1/auth/me", Some(&token), None).await.0,
        StatusCode::OK
    );
    assert_eq!(
        call(&app, "POST", "/v1/auth/logout", Some(&token), None)
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        call(&app, "GET", "/v1/auth/me", Some(&token), None).await.0,
        StatusCode::UNAUTHORIZED,
        "the token still worked after logout"
    );
}

#[tokio::test]
async fn logging_out_twice_is_not_an_error() {
    let Some((app, _db)) = app().await else {
        return;
    };
    let (_, body) = signup(&app, &email(), "a-long-enough-password").await;
    let token = body["token"].as_str().unwrap().to_string();
    assert_eq!(
        call(&app, "POST", "/v1/auth/logout", Some(&token), None)
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        call(&app, "POST", "/v1/auth/logout", Some(&token), None)
            .await
            .0,
        StatusCode::NO_CONTENT
    );
}

#[tokio::test]
async fn changing_the_password_revokes_every_session() {
    // If the password was changed because it leaked, sessions an attacker already holds have to die
    // with it. Otherwise the change accomplishes nothing.
    let Some((app, _db)) = app().await else {
        return;
    };
    let e = email();
    let (_, a) = signup(&app, &e, "a-long-enough-password").await;
    let session_one = a["token"].as_str().unwrap().to_string();
    let (_, b) = login(&app, &e, "a-long-enough-password").await;
    let session_two = b["token"].as_str().unwrap().to_string();

    let (status, _) = call(
        &app,
        "POST",
        "/v1/auth/password",
        Some(&session_one),
        Some(json!({"current_password": "a-long-enough-password", "new_password": "an-even-longer-password"})),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    for (label, t) in [
        ("the caller's own", &session_one),
        ("another", &session_two),
    ] {
        assert_eq!(
            call(&app, "GET", "/v1/auth/me", Some(t), None).await.0,
            StatusCode::UNAUTHORIZED,
            "{label} session survived a password change"
        );
    }
    assert_eq!(
        login(&app, &e, "an-even-longer-password").await.0,
        StatusCode::OK
    );
    assert_eq!(
        login(&app, &e, "a-long-enough-password").await.0,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn changing_a_password_requires_the_current_one() {
    // Otherwise a stolen session token becomes a permanent account takeover.
    let Some((app, _db)) = app().await else {
        return;
    };
    let e = email();
    let (_, body) = signup(&app, &e, "a-long-enough-password").await;
    let token = body["token"].as_str().unwrap().to_string();

    let (status, _) = call(
        &app,
        "POST",
        "/v1/auth/password",
        Some(&token),
        Some(json!({"current_password": "not-the-password", "new_password": "an-even-longer-password"})),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        login(&app, &e, "a-long-enough-password").await.0,
        StatusCode::OK,
        "the old password stopped working anyway"
    );
}

// ---------------------------------------------------------------- mode isolation

#[tokio::test]
async fn a_local_session_is_worthless_under_jwks_mode() {
    // The swap has to be complete. A token minted by the mode we are not running must not verify,
    // or turning on an external provider would leave the old way in.
    let Some((local_app, _db)) = app_with(AuthMode::Local).await else {
        return;
    };
    let (_, body) = signup(&local_app, &email(), "a-long-enough-password").await;
    let token = body["token"].as_str().unwrap().to_string();

    let Some((jwks_app, _db2)) = app_with(AuthMode::Jwks).await else {
        return;
    };
    assert_eq!(
        call(&jwks_app, "GET", "/v1/projects", Some(&token), None)
            .await
            .0,
        StatusCode::UNAUTHORIZED,
        "a locally issued session verified against the external provider"
    );
}

#[tokio::test]
async fn the_local_routes_disappear_under_jwks_mode() {
    let Some((app, _db)) = app_with(AuthMode::Jwks).await else {
        return;
    };
    let (status, _) = signup(&app, &email(), "a-long-enough-password").await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "signup was reachable while using an external provider"
    );
    assert_eq!(
        login(&app, "a@b.com", "a-long-enough-password").await.0,
        StatusCode::NOT_FOUND
    );
}
