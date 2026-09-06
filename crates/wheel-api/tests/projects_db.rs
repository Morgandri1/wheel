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
        auth_mode: wheel_api::config::AuthMode::Jwks,
        session_secret: wheel_api::crypto::Secret::new("test-session-secret-at-least-32-chars"),
        master_key: [3u8; 32],
        host_url: "http://host.invalid".into(),
        host_secret: Secret::new("host-secret"),
        engine_port: 7000,
        public_base_url: "http://localhost".into(),
        max_projects_per_user: 3,
        ingress_rate_per_min: 60,
        ingress_body_limit_bytes: 5 * 1024 * 1024,
        proxy_timeout_secs: 30,
        host_connect_timeout_secs: 3,
    }
}

/// A fresh subject for each test.
///
/// These tests share one database, so a fixed id lets rows from earlier tests — and from earlier
/// *runs* — pile up under the same user. That silently breaks every assertion of the form "what
/// does this user have": the cap test hit its limit on leftovers, and the list test saw six
/// projects where it expected one. A unique id per call keeps tests isolated and idempotent
/// without truncating tables that other tests may be using concurrently.
fn user(label: &str) -> String {
    format!("user_{label}_{}", uuid::Uuid::new_v4())
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

/// Fresh data per test, so tests cannot see each other's rows.
///
/// Tests that use this must run serially (`--test-threads=1`), since they share one database and
/// each one truncates it.
/// Prepare the schema exactly once per test binary, however many tests ask for it.
///
/// This used to drop and re-migrate on every call. Tests run in parallel, so that meant one test
/// truncating the world while another was mid-request — the suite failed a different assertion on
/// roughly every other run. A flaky security suite is worse than no suite, because people learn to
/// read red as noise.
///
/// Isolation now comes from each test using a unique subject (see `user()`) rather than from
/// destroying shared state, so concurrent tests simply cannot see each other's rows.
///
/// Reset the schema exactly once per test binary, not once per `app()` call.
///
/// Doing it per call was a real bug, not just waste. Tests run in parallel, so one test's
/// `DROP TABLE ... CASCADE` deleted the tables out from under another test that was mid-request.
/// The result was an intermittent failure in whichever test happened to be running — usually
/// `stranger_cannot_mutate_or_proxy` — which reads as a flaky *security* check. Nothing is more
/// corrosive than a boundary test that cries wolf, so the destructive reset is serialised here and
/// per-test isolation comes from unique subjects instead.
async fn app() -> Option<(axum::Router, sqlx::PgPool)> {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();

    // Skipping is a convenience for a laptop without a database, never for CI. These tests are the
    // only thing covering the ownership boundary end to end, and a suite that quietly asserts
    // nothing while reporting green is worse than one that fails — that is exactly how this file
    // sat broken while `cargo test` stayed green.
    let url = match std::env::var("TEST_DATABASE_URL") {
        Ok(u) => u,
        // Gated on the database being promised, not on being in CI. Keying this off `CI` asserted
        // that every CI job has Postgres, which was not true and turned main red — the check has to
        // depend on the thing it actually needs.
        Err(_) if std::env::var("WHEEL_CI_HAS_DB").as_deref() == Ok("1") => {
            panic!(
                "WHEEL_CI_HAS_DB=1 but TEST_DATABASE_URL is unset: these tests are the only \
                 end-to-end cover for the ownership boundary and must not skip here"
            )
        }
        Err(_) => {
            eprintln!("skipping {}: TEST_DATABASE_URL not set", module_path!());
            return None;
        }
    };

    // A pool per test, deliberately, and a small one.
    //
    // The obvious optimisation — one `static` pool shared by the whole binary — is wrong here, and
    // silently so. Each `#[tokio::test]` runs on its own runtime; a shared pool gets bound to
    // whichever runtime happened to create it, and once that runtime shuts down every later test
    // sees "A Tokio 1.x context was found, but it is being shutdown" followed by pool timeouts.
    // That failure is intermittent and reads like a boundary bug, which is the worst kind of
    // fixture defect. Two connections per test keeps the total well inside the server's limit.
    let db = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .acquire_timeout(std::time::Duration::from_secs(10))
        .connect(&url)
        .await
        .expect("connect to TEST_DATABASE_URL");

    // Idempotent, so it is safe to run per test. Tests isolate from each other by using a unique
    // subject per test (see `user()`) rather than by truncating shared tables, so no test needs a
    // clean slate and none can pull the schema out from under another.
    sqlx::migrate!("./migrations")
        .run(&db)
        .await
        .expect("run migrations");

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
        auth_limiter: wheel_api::http::authlimit::AuthLimiter::new(1000, 1000),
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
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
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
    let alice = token(&user("alice"));
    let mallory = token(&user("mallory"));

    let (status, proj) = send(
        &app,
        "POST",
        "/v1/projects",
        Some(&alice),
        Some(json!({"name":"alice board"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{proj}");
    let id = proj["id"].as_str().unwrap().to_string();

    let (status, _) = send(
        &app,
        "GET",
        &format!("/v1/projects/{id}"),
        Some(&alice),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "owner cannot read own project");

    // The whole ballgame: a *valid* token for the wrong user must not distinguish "yours but
    // forbidden" from "does not exist".
    let (status, body) = send(
        &app,
        "GET",
        &format!("/v1/projects/{id}"),
        Some(&mallory),
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "cross-tenant read leaked: {body}"
    );
    assert_eq!(body["error"]["code"], "not_found");

    // ...and it must match the response for an id that genuinely does not exist, byte for byte.
    let ghost = uuid::Uuid::new_v4();
    let (ghost_status, ghost_body) = send(
        &app,
        "GET",
        &format!("/v1/projects/{ghost}"),
        Some(&mallory),
        None,
    )
    .await;
    assert_eq!(ghost_status, status);
    assert_eq!(ghost_body, body, "existence oracle: the two 404s differ");
}

#[tokio::test]
async fn stranger_cannot_mutate_or_proxy() {
    let (app, _db) = app_or_skip!();
    let alice = token(&user("alice"));
    let mallory = token(&user("mallory"));

    let (_, proj) = send(
        &app,
        "POST",
        "/v1/projects",
        Some(&alice),
        Some(json!({"name":"a"})),
    )
    .await;
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
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "{method} {path} was reachable by a stranger"
        );
    }

    // Alice's project is untouched.
    let (_, after) = send(
        &app,
        "GET",
        &format!("/v1/projects/{id}"),
        Some(&alice),
        None,
    )
    .await;
    assert_eq!(after["name"], "a");
}

#[tokio::test]
async fn unauthenticated_requests_are_rejected() {
    let (app, _db) = app_or_skip!();
    let alice = token(&user("alice"));
    let (_, proj) = send(
        &app,
        "POST",
        "/v1/projects",
        Some(&alice),
        Some(json!({"name":"a"})),
    )
    .await;
    let id = proj["id"].as_str().unwrap().to_string();

    for path in [
        "/v1/projects".to_string(),
        format!("/v1/projects/{id}"),
        format!("/v1/projects/{id}/engine/v1/board"),
    ] {
        let (status, _) = send(&app, "GET", &path, None, None).await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "{path} served without a token"
        );
    }
}

#[tokio::test]
async fn list_is_scoped_to_the_caller() {
    let (app, _db) = app_or_skip!();
    let alice = token(&user("alice"));
    let mallory = token(&user("mallory"));

    send(
        &app,
        "POST",
        "/v1/projects",
        Some(&alice),
        Some(json!({"name":"alice one"})),
    )
    .await;
    send(
        &app,
        "POST",
        "/v1/projects",
        Some(&mallory),
        Some(json!({"name":"mallory one"})),
    )
    .await;

    let (status, list) = send(&app, "GET", "/v1/projects", Some(&alice), None).await;
    assert_eq!(status, StatusCode::OK);
    let names: Vec<&str> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        vec!["alice one"],
        "list leaked another user's projects"
    );
}

#[tokio::test]
async fn malformed_project_id_is_a_400_not_a_500() {
    let (app, _db) = app_or_skip!();
    let alice = token(&user("alice"));
    for bad in ["not-a-uuid", "../../etc/passwd", "00000000", "%00"] {
        let (status, _) = send(
            &app,
            "GET",
            &format!("/v1/projects/{bad}"),
            Some(&alice),
            None,
        )
        .await;
        assert!(
            status == StatusCode::BAD_REQUEST || status == StatusCode::NOT_FOUND,
            "id {bad:?} produced {status}"
        );
    }
}

#[tokio::test]
async fn project_cap_is_enforced() {
    let (app, _db) = app_or_skip!();
    let alice = token(&user("alice"));
    for i in 0..3 {
        let (status, _) = send(
            &app,
            "POST",
            "/v1/projects",
            Some(&alice),
            Some(json!({"name": format!("p{i}")})),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
    }
    let (status, body) = send(
        &app,
        "POST",
        "/v1/projects",
        Some(&alice),
        Some(json!({"name":"one too many"})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
}

#[tokio::test]
async fn invalid_names_are_rejected() {
    let (app, _db) = app_or_skip!();
    let alice = token(&user("alice"));
    for bad in ["", "   ", "a\nb", &"x".repeat(65)] {
        let (status, _) = send(
            &app,
            "POST",
            "/v1/projects",
            Some(&alice),
            Some(json!({"name": bad})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "accepted name {bad:?}");
    }
}

#[tokio::test]
async fn ingress_is_closed_until_opted_in() {
    let (app, _db) = app_or_skip!();
    let alice = token(&user("alice"));
    let (_, proj) = send(
        &app,
        "POST",
        "/v1/projects",
        Some(&alice),
        Some(json!({"name":"a"})),
    )
    .await;
    let id = proj["id"].as_str().unwrap().to_string();

    // Default is closed.
    let (status, _) = send(&app, "GET", &format!("/p/{id}/hello"), None, None).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "ingress was open by default");

    // Unknown project is 404, not 403 — 403 would confirm the id is real.
    let ghost = uuid::Uuid::new_v4();
    let (status, _) = send(&app, "GET", &format!("/p/{ghost}/hello"), None, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// --- WebSocket tickets ---------------------------------------------------------------------
//
// The ticket is the one credential we deliberately allow into a URL, so its guarantees carry more
// weight than usual: it must be usable exactly once, expire quickly, and be useless against any
// project other than the one it was minted for.

#[tokio::test]
async fn ws_ticket_is_single_use() {
    let Some((app, _db)) = app().await else {
        return;
    };
    let alice = token(&user("alice"));

    let (status, proj) = send(
        &app,
        "POST",
        "/v1/projects",
        Some(&alice),
        Some(json!({"name":"ws single use"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let id = proj["id"].as_str().unwrap().to_string();

    let (status, body) = send(
        &app,
        "POST",
        &format!("/v1/projects/{id}/ws-ticket"),
        Some(&alice),
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "minting a ticket for my own project should succeed"
    );
    let ticket = body["ticket"].as_str().unwrap().to_string();
    assert_eq!(body["expires_in"], 30);

    // First redemption wins.
    let first = send(
        &app,
        "GET",
        &format!("/v1/projects/{id}/engine/v1/events?ticket={ticket}"),
        None,
        None,
    )
    .await;
    assert_ne!(
        first.0,
        StatusCode::UNAUTHORIZED,
        "a fresh ticket must authenticate; got {:?}",
        first
    );

    // Second must not. It fails as 401 rather than reaching the engine at all.
    let (status, _) = send(
        &app,
        "GET",
        &format!("/v1/projects/{id}/engine/v1/events?ticket={ticket}"),
        None,
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "a ticket was accepted twice"
    );
}

#[tokio::test]
async fn ws_ticket_does_not_open_another_project() {
    let Some((app, _db)) = app().await else {
        return;
    };
    let alice = token(&user("alice"));

    let mut ids = Vec::new();
    for name in ["ws project a", "ws project b"] {
        let (status, proj) = send(
            &app,
            "POST",
            "/v1/projects",
            Some(&alice),
            Some(json!({"name": name})),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        ids.push(proj["id"].as_str().unwrap().to_string());
    }

    let (_, body) = send(
        &app,
        "POST",
        &format!("/v1/projects/{}/ws-ticket", ids[0]),
        Some(&alice),
        None,
    )
    .await;
    let ticket = body["ticket"].as_str().unwrap().to_string();

    // Same owner, different project: still refused. Ownership is not the only binding.
    let (status, _) = send(
        &app,
        "GET",
        &format!("/v1/projects/{}/engine/v1/events?ticket={ticket}", ids[1]),
        None,
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "a ticket opened a different project's socket"
    );
}

#[tokio::test]
async fn ws_ticket_cannot_be_minted_for_someone_elses_project() {
    let Some((app, _db)) = app().await else {
        return;
    };
    let alice = token(&user("alice"));
    let mallory = token(&user("mallory"));

    let (_, proj) = send(
        &app,
        "POST",
        "/v1/projects",
        Some(&alice),
        Some(json!({"name":"alice ws"})),
    )
    .await;
    let id = proj["id"].as_str().unwrap().to_string();

    let (status, _) = send(
        &app,
        "POST",
        &format!("/v1/projects/{id}/ws-ticket"),
        Some(&mallory),
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "a stranger minted a ticket for someone else's project"
    );
}

#[tokio::test]
async fn garbage_and_missing_tickets_are_refused() {
    let Some((app, _db)) = app().await else {
        return;
    };
    let alice = token(&user("alice"));
    let (_, proj) = send(
        &app,
        "POST",
        "/v1/projects",
        Some(&alice),
        Some(json!({"name":"ws garbage"})),
    )
    .await;
    let id = proj["id"].as_str().unwrap().to_string();

    for q in ["?ticket=not-a-real-ticket", "?ticket=", ""] {
        let (status, _) = send(
            &app,
            "GET",
            &format!("/v1/projects/{id}/engine/v1/events{q}"),
            None,
            None,
        )
        .await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "unauthenticated events access via {q:?}"
        );
    }
}
