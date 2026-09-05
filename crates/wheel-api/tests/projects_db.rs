//! Router-level integration tests against a real Postgres.
//!
//! The auth unit tests prove the *verifier* is sound. These prove the *boundary* is wired: that a
//! request carrying a perfectly valid token for the wrong user cannot reach another user's project
//! through any route, and that the two "you may not have this" cases are indistinguishable.
//!
//! Skipped when `TEST_DATABASE_URL` is unset so the suite stays runnable without a database:
//!   TEST_DATABASE_URL=postgres://wheel:wheel@localhost:55432/wheel_test cargo test -p wheel-api

use axum::body::Body;
use axum::http::{Request, StatusCode};
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use serde_json::json;
use tower::ServiceExt;
use wheel_api::config::{Config, Env};
use wheel_api::crypto::Secret;
use wheel_api::orchestrator::{NoopOrchestrator, Orchestrator};
use wheel_api::state::{AppState, Inner};

const ISSUER: &str = "https://dev.wheel.local";
const DEV_SECRET: &str = "integration-test-secret";

fn dev_config(db_url: &str) -> Config {
    Config {
        env: Env::Dev,
        bind_addr: "127.0.0.1:0".into(),
        database_url: db_url.into(),
        clerk_jwks_url: "http://unused.invalid/jwks".into(),
        clerk_issuer: ISSUER.into(),
        clerk_azp: vec![],
        dev_secret: Some(DEV_SECRET.into()),
        master_key: [3u8; 32],
        host_url: "http://host.invalid".into(),
        host_secret: Secret::new("host-secret"),
        engine_port: 7000,
        public_base_url: "http://localhost".into(),
        max_projects_per_user: 3,
        ingress_rate_per_min: 60,
        ingress_body_limit_bytes: 5 * 1024 * 1024,
        proxy_timeout_secs: 30,
    }
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
        &C { sub, iss: ISSUER, exp: now + 3600, nbf: now - 60 },
        &EncodingKey::from_secret(DEV_SECRET.as_bytes()),
    )
    .unwrap()
}

/// Fresh data per test, so tests cannot see each other's rows.
///
/// Tests that use this must run serially (`--test-threads=1`), since they share one database and
/// each one truncates it.
async fn app() -> Option<(axum::Router, sqlx::PgPool)> {
    let url = std::env::var("TEST_DATABASE_URL").ok()?;
    let db = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(&url)
        .await
        .expect("connect to TEST_DATABASE_URL");

    // Drop the migration ledger along with the tables. Dropping only the tables would leave
    // `_sqlx_migrations` claiming 0001 was already applied, so the migration is skipped and the
    // tables never come back — every subsequent query then fails on a missing relation.
    sqlx::query(
        "DROP TABLE IF EXISTS ingress_rate_limits, project_secrets, projects, _sqlx_migrations CASCADE",
    )
    .execute(&db)
    .await
    .unwrap();
    sqlx::migrate!("./migrations").run(&db).await.unwrap();

    let cfg = dev_config(&url);
    let state = AppState::new(Inner {
        jwks: wheel_api::auth::jwks::JwksCache::new(
            cfg.clerk_jwks_url.clone(),
            reqwest::Client::new(),
        ),
        cfg,
        db: db.clone(),
        http: reqwest::Client::new(),
        orch: std::sync::Arc::new(NoopOrchestrator) as std::sync::Arc<dyn Orchestrator>,
        ingress_limiter: wheel_api::http::ratelimit::RateLimiter::new(60),
        engine_base_override: None,
    });
    Some((wheel_api::build_router(state, &[]), db))
}

async fn send(
    app: &axum::Router,
    method: &str,
    path: &str,
    tok: Option<&str>,
    body: Option<serde_json::Value>,
) -> (StatusCode, serde_json::Value) {
    let mut req = Request::builder().method(method).uri(path);
    if let Some(t) = tok {
        req = req.header("x-auth-token", t);
    }
    let req = match body {
        Some(b) => req
            .header("content-type", "application/json")
            .body(Body::from(b.to_string()))
            .unwrap(),
        None => req.body(Body::empty()).unwrap(),
    };
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20).await.unwrap();
    let value = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, value)
}

macro_rules! app_or_skip {
    () => {
        match app().await {
            Some(v) => v,
            None => {
                eprintln!("skipping: TEST_DATABASE_URL not set");
                return;
            }
        }
    };
}

#[tokio::test]
async fn owner_sees_own_project_and_stranger_gets_404() {
    let (app, _db) = app_or_skip!();
    let alice = token("user_alice");
    let mallory = token("user_mallory");

    let (status, proj) = send(&app, "POST", "/v1/projects", Some(&alice), Some(json!({"name":"alice board"}))).await;
    assert_eq!(status, StatusCode::CREATED, "{proj}");
    let id = proj["id"].as_str().unwrap().to_string();

    let (status, _) = send(&app, "GET", &format!("/v1/projects/{id}"), Some(&alice), None).await;
    assert_eq!(status, StatusCode::OK, "owner cannot read own project");

    // The whole ballgame: a *valid* token for the wrong user must not distinguish "yours but
    // forbidden" from "does not exist".
    let (status, body) = send(&app, "GET", &format!("/v1/projects/{id}"), Some(&mallory), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "cross-tenant read leaked: {body}");
    assert_eq!(body["error"]["code"], "not_found");

    // ...and it must match the response for an id that genuinely does not exist, byte for byte.
    let ghost = uuid::Uuid::new_v4();
    let (ghost_status, ghost_body) =
        send(&app, "GET", &format!("/v1/projects/{ghost}"), Some(&mallory), None).await;
    assert_eq!(ghost_status, status);
    assert_eq!(ghost_body, body, "existence oracle: the two 404s differ");
}

#[tokio::test]
async fn stranger_cannot_mutate_or_proxy() {
    let (app, _db) = app_or_skip!();
    let alice = token("user_alice");
    let mallory = token("user_mallory");

    let (_, proj) = send(&app, "POST", "/v1/projects", Some(&alice), Some(json!({"name":"a"}))).await;
    let id = proj["id"].as_str().unwrap().to_string();

    for (method, path) in [
        ("PATCH", format!("/v1/projects/{id}")),
        ("DELETE", format!("/v1/projects/{id}")),
        ("POST", format!("/v1/projects/{id}/start")),
        ("POST", format!("/v1/projects/{id}/stop")),
        ("POST", format!("/v1/projects/{id}/restart")),
        ("GET", format!("/v1/projects/{id}/engine/v1/board")),
    ] {
        let body = (method == "PATCH").then(|| json!({"name": "stolen"}));
        let (status, _) = send(&app, method, &path, Some(&mallory), body).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{method} {path} was reachable by a stranger");
    }

    // Alice's project is untouched.
    let (_, after) = send(&app, "GET", &format!("/v1/projects/{id}"), Some(&alice), None).await;
    assert_eq!(after["name"], "a");
}

#[tokio::test]
async fn unauthenticated_requests_are_rejected() {
    let (app, _db) = app_or_skip!();
    let alice = token("user_alice");
    let (_, proj) = send(&app, "POST", "/v1/projects", Some(&alice), Some(json!({"name":"a"}))).await;
    let id = proj["id"].as_str().unwrap().to_string();

    for path in [
        "/v1/projects".to_string(),
        format!("/v1/projects/{id}"),
        format!("/v1/projects/{id}/engine/v1/board"),
    ] {
        let (status, _) = send(&app, "GET", &path, None, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{path} served without a token");
    }
}

#[tokio::test]
async fn list_is_scoped_to_the_caller() {
    let (app, _db) = app_or_skip!();
    let alice = token("user_alice");
    let mallory = token("user_mallory");

    send(&app, "POST", "/v1/projects", Some(&alice), Some(json!({"name":"alice one"}))).await;
    send(&app, "POST", "/v1/projects", Some(&mallory), Some(json!({"name":"mallory one"}))).await;

    let (status, list) = send(&app, "GET", "/v1/projects", Some(&alice), None).await;
    assert_eq!(status, StatusCode::OK);
    let names: Vec<&str> = list.as_array().unwrap().iter().map(|p| p["name"].as_str().unwrap()).collect();
    assert_eq!(names, vec!["alice one"], "list leaked another user's projects");
}

#[tokio::test]
async fn malformed_project_id_is_a_400_not_a_500() {
    let (app, _db) = app_or_skip!();
    let alice = token("user_alice");
    for bad in ["not-a-uuid", "../../etc/passwd", "00000000", "%00"] {
        let (status, _) = send(&app, "GET", &format!("/v1/projects/{bad}"), Some(&alice), None).await;
        assert!(
            status == StatusCode::BAD_REQUEST || status == StatusCode::NOT_FOUND,
            "id {bad:?} produced {status}"
        );
    }
}

#[tokio::test]
async fn project_cap_is_enforced() {
    let (app, _db) = app_or_skip!();
    let alice = token("user_alice");
    for i in 0..3 {
        let (status, _) = send(&app, "POST", "/v1/projects", Some(&alice), Some(json!({"name": format!("p{i}")}))).await;
        assert_eq!(status, StatusCode::CREATED);
    }
    let (status, body) = send(&app, "POST", "/v1/projects", Some(&alice), Some(json!({"name":"one too many"}))).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
}

#[tokio::test]
async fn invalid_names_are_rejected() {
    let (app, _db) = app_or_skip!();
    let alice = token("user_alice");
    for bad in ["", "   ", "a\nb", &"x".repeat(65)] {
        let (status, _) = send(&app, "POST", "/v1/projects", Some(&alice), Some(json!({"name": bad}))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "accepted name {bad:?}");
    }
}

#[tokio::test]
async fn ingress_is_closed_until_opted_in() {
    let (app, _db) = app_or_skip!();
    let alice = token("user_alice");
    let (_, proj) = send(&app, "POST", "/v1/projects", Some(&alice), Some(json!({"name":"a"}))).await;
    let id = proj["id"].as_str().unwrap().to_string();

    // Default is closed.
    let (status, _) = send(&app, "GET", &format!("/p/{id}/hello"), None, None).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "ingress was open by default");

    // Unknown project is 404, not 403 — 403 would confirm the id is real.
    let ghost = uuid::Uuid::new_v4();
    let (status, _) = send(&app, "GET", &format!("/p/{ghost}/hello"), None, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
