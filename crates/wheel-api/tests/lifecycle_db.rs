//! Project lifecycle: what the API reports as `status`, and when.
//!
//! The interesting cases are the disagreements. The orchestrator is another process; it can say a
//! start succeeded and then report the sandbox as stopped, or fail to answer at all. What the API
//! tells the user in those moments determines whether a UI polls forever or shows a fault.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tower::ServiceExt;
use uuid::Uuid;
use wheel_api::config::{Config, Env};
use wheel_api::crypto::Secret;
use wheel_api::models::ProjectStatus;
use wheel_api::orchestrator::{EngineSecrets, HostRefusal, Orchestrator};
use wheel_api::state::{AppState, Inner};

const ISSUER: &str = "https://dev.wheel.local";
const DEV_SECRET: &str = "integration-test-secret";

/// An orchestrator whose answers the test dictates.
#[derive(Clone)]
struct FakeOrch {
    status: Arc<Mutex<ProjectStatus>>,
    fail_start: Arc<Mutex<bool>>,
    out_of_disk: Arc<Mutex<bool>>,
    fail_destroy: Arc<Mutex<bool>>,
    status_errors: Arc<Mutex<bool>>,
    destroyed: Arc<AtomicUsize>,
}

impl Default for FakeOrch {
    fn default() -> Self {
        Self {
            // Stopped is the honest default for a sandbox nothing has started.
            status: Arc::new(Mutex::new(ProjectStatus::Stopped)),
            fail_start: Arc::new(Mutex::new(false)),
            out_of_disk: Arc::new(Mutex::new(false)),
            fail_destroy: Arc::new(Mutex::new(false)),
            status_errors: Arc::new(Mutex::new(false)),
            destroyed: Arc::new(AtomicUsize::new(0)),
        }
    }
}

#[async_trait::async_trait]
impl Orchestrator for FakeOrch {
    async fn host_alive(&self) -> anyhow::Result<()> {
        Ok(())
    }

    async fn provision(&self, _: &Uuid, _: &EngineSecrets) -> anyhow::Result<()> {
        Ok(())
    }
    async fn start(&self, _: &Uuid) -> anyhow::Result<()> {
        if *self.out_of_disk.lock().unwrap() {
            // Wrapped in context, as the real host client does, so the test also proves the
            // downcast survives the chain rather than only working on a bare error.
            return Err(anyhow::Error::new(HostRefusal::OutOfDisk)
                .context("host returned 507 Insufficient Storage"));
        }
        if *self.fail_start.lock().unwrap() {
            anyhow::bail!("host refused to start the sandbox");
        }
        Ok(())
    }
    async fn stop(&self, _: &Uuid) -> anyhow::Result<()> {
        Ok(())
    }
    async fn restart(&self, _: &Uuid) -> anyhow::Result<()> {
        Ok(())
    }
    async fn destroy(&self, _: &Uuid) -> anyhow::Result<()> {
        if *self.fail_destroy.lock().unwrap() {
            anyhow::bail!("host could not destroy the sandbox");
        }
        self.destroyed.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    async fn status(&self, _: &Uuid) -> anyhow::Result<ProjectStatus> {
        if *self.status_errors.lock().unwrap() {
            anyhow::bail!("host unreachable");
        }
        Ok(*self.status.lock().unwrap())
    }
}

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
        master_key: [5u8; 32],
        host_url: "http://host.invalid".into(),
        host_secret: Secret::new("host-secret"),
        engine_port: 7000,
        public_base_url: "http://localhost".into(),
        max_projects_per_user: 20,
        ingress_rate_per_min: 60,
        ingress_body_limit_bytes: 5 * 1024 * 1024,
        proxy_timeout_secs: 30,
        host_connect_timeout_secs: 3,
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

async fn app(orch: FakeOrch) -> Option<(Router, String)> {
    let url = match std::env::var("TEST_DATABASE_URL") {
        Ok(u) => u,
        // Keyed on a promised database rather than on CI: not every CI job has Postgres.
        Err(_) if std::env::var("WHEEL_CI_HAS_DB").as_deref() == Ok("1") => {
            panic!("WHEEL_CI_HAS_DB=1 but TEST_DATABASE_URL is unset")
        }
        Err(_) => {
            eprintln!("skipping {}: TEST_DATABASE_URL not set", module_path!());
            return None;
        }
    };
    let db = wheel_api::db::Db::connect(&url)
        .await
        .expect("connect and migrate");

    let state = AppState::new(Inner {
        jwks: wheel_api::auth::jwks::JwksCache::new(
            "http://unused.invalid/jwks".into(),
            reqwest::Client::new(),
        ),
        cfg: cfg(&url),
        db,
        http: reqwest::Client::new(),
        orch: Arc::new(orch) as Arc<dyn Orchestrator>,
        ingress_limiter: wheel_api::http::ratelimit::RateLimiter::new(60),
        auth_limiter: wheel_api::http::authlimit::AuthLimiter::new(1000, 1000),
        engine_base_override: None,
    });
    Some((
        wheel_api::build_router(state, &[]),
        format!("user_{}", Uuid::new_v4()),
    ))
}

async fn call(
    app: &Router,
    method: &str,
    path: &str,
    tok: &str,
    body: Option<serde_json::Value>,
) -> (StatusCode, serde_json::Value) {
    let b = Request::builder()
        .method(method)
        .uri(path)
        .header("x-auth-token", tok);
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

async fn project(app: &Router, tok: &str) -> String {
    let (s, v) = call(
        app,
        "POST",
        "/v1/projects",
        tok,
        Some(json!({"name": "lc"})),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    v["id"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn start_reports_running_when_the_sandbox_is_up() {
    let orch = FakeOrch {
        status: Arc::new(Mutex::new(ProjectStatus::Running)),
        ..Default::default()
    };
    let Some((app, u)) = app(orch).await else {
        return;
    };
    let tok = token(&u);
    let id = project(&app, &tok).await;

    let (s, v) = call(
        &app,
        "POST",
        &format!("/v1/projects/{id}/start"),
        &tok,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["status"], "running");
}

#[tokio::test]
async fn a_start_that_leaves_the_sandbox_stopped_is_an_error_not_stopped() {
    // The disagreement case. "stopped" would invite a poll loop that never terminates, because
    // nothing further is going to happen.
    let orch = FakeOrch {
        status: Arc::new(Mutex::new(ProjectStatus::Stopped)),
        ..Default::default()
    };
    let Some((app, u)) = app(orch).await else {
        return;
    };
    let tok = token(&u);
    let id = project(&app, &tok).await;

    let (s, v) = call(
        &app,
        "POST",
        &format!("/v1/projects/{id}/start"),
        &tok,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["status"], "error");
}

#[tokio::test]
async fn a_status_probe_that_fails_leaves_the_project_starting() {
    // We cannot see the sandbox, but we did ask the host to start it. "starting" is the honest
    // answer; "error" would be a claim we have not earned.
    let orch = FakeOrch {
        status_errors: Arc::new(Mutex::new(true)),
        ..Default::default()
    };
    let Some((app, u)) = app(orch).await else {
        return;
    };
    let tok = token(&u);
    let id = project(&app, &tok).await;

    let (_, v) = call(
        &app,
        "POST",
        &format!("/v1/projects/{id}/start"),
        &tok,
        None,
    )
    .await;
    assert_eq!(v["status"], "starting");
}

#[tokio::test]
async fn a_host_that_refuses_to_start_is_a_500_not_a_silent_success() {
    let orch = FakeOrch {
        fail_start: Arc::new(Mutex::new(true)),
        ..Default::default()
    };
    let Some((app, u)) = app(orch).await else {
        return;
    };
    let tok = token(&u);
    let id = project(&app, &tok).await;

    let (s, _) = call(
        &app,
        "POST",
        &format!("/v1/projects/{id}/start"),
        &tok,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::INTERNAL_SERVER_ERROR);
}

#[tokio::test]
async fn stop_and_restart_report_their_outcome() {
    let orch = FakeOrch {
        status: Arc::new(Mutex::new(ProjectStatus::Running)),
        ..Default::default()
    };
    let Some((app, u)) = app(orch).await else {
        return;
    };
    let tok = token(&u);
    let id = project(&app, &tok).await;

    let (s, v) = call(&app, "POST", &format!("/v1/projects/{id}/stop"), &tok, None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["status"], "stopped");

    let (s, v) = call(
        &app,
        "POST",
        &format!("/v1/projects/{id}/restart"),
        &tok,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["status"], "running");
}

#[tokio::test]
async fn get_reconciles_the_stored_status_against_the_runtime() {
    // A container can die without telling us. Reading the project should not keep reporting the
    // status we last wrote down.
    let orch = FakeOrch {
        status: Arc::new(Mutex::new(ProjectStatus::Running)),
        ..Default::default()
    };
    let Some((app, u)) = app(orch.clone()).await else {
        return;
    };
    let tok = token(&u);
    let id = project(&app, &tok).await;
    call(
        &app,
        "POST",
        &format!("/v1/projects/{id}/start"),
        &tok,
        None,
    )
    .await;

    *orch.status.lock().unwrap() = ProjectStatus::Stopped;
    let (_, v) = call(&app, "GET", &format!("/v1/projects/{id}"), &tok, None).await;
    assert_eq!(
        v["status"], "stopped",
        "a dead sandbox should not still read as running"
    );
}

#[tokio::test]
async fn delete_tears_down_the_sandbox_before_dropping_the_row() {
    let orch = FakeOrch::default();
    let Some((app, u)) = app(orch.clone()).await else {
        return;
    };
    let tok = token(&u);
    let id = project(&app, &tok).await;

    let (s, _) = call(&app, "DELETE", &format!("/v1/projects/{id}"), &tok, None).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    assert_eq!(orch.destroyed.load(Ordering::SeqCst), 1);

    let (s, _) = call(&app, "GET", &format!("/v1/projects/{id}"), &tok, None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_failed_teardown_keeps_the_row() {
    // If the sandbox could not be destroyed, deleting the row would strand it: a container nobody
    // has a record of is a container nobody will ever clean up.
    let orch = FakeOrch {
        fail_destroy: Arc::new(Mutex::new(true)),
        ..Default::default()
    };
    let Some((app, u)) = app(orch).await else {
        return;
    };
    let tok = token(&u);
    let id = project(&app, &tok).await;

    let (s, _) = call(&app, "DELETE", &format!("/v1/projects/{id}"), &tok, None).await;
    assert_eq!(s, StatusCode::INTERNAL_SERVER_ERROR);

    let (s, _) = call(&app, "GET", &format!("/v1/projects/{id}"), &tok, None).await;
    assert_eq!(s, StatusCode::OK, "the row must survive a failed teardown");
}

#[tokio::test]
async fn patch_updates_name_and_capabilities_independently() {
    let orch = FakeOrch::default();
    let Some((app, u)) = app(orch).await else {
        return;
    };
    let tok = token(&u);
    let id = project(&app, &tok).await;

    let (_, v) = call(
        &app,
        "PATCH",
        &format!("/v1/projects/{id}"),
        &tok,
        Some(json!({"name": "renamed"})),
    )
    .await;
    assert_eq!(v["name"], "renamed");
    assert_eq!(
        v["capabilities"]["http"], false,
        "an omitted field must be left alone"
    );

    let (_, v) = call(
        &app,
        "PATCH",
        &format!("/v1/projects/{id}"),
        &tok,
        Some(json!({"capabilities": {"http": true}})),
    )
    .await;
    assert_eq!(v["capabilities"]["http"], true);
    assert_eq!(v["name"], "renamed", "an omitted field must be left alone");
}

#[tokio::test]
async fn healthz_needs_no_credentials() {
    let Some((app, _)) = app(FakeOrch::default()).await else {
        return;
    };
    let req = Request::builder()
        .uri("/healthz")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn creating_a_project_starts_its_sandbox() {
    let orch = FakeOrch {
        status: Arc::new(Mutex::new(ProjectStatus::Running)),
        ..Default::default()
    };
    let Some((app, u)) = app(orch).await else {
        return;
    };
    let tok = token(&u);

    let (s, v) = call(
        &app,
        "POST",
        "/v1/projects",
        &tok,
        Some(json!({"name": "new"})),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    assert_eq!(
        v["status"], "running",
        "a project nobody has started yet is a product that does nothing on first use"
    );

    let (_, v) = call(
        &app,
        "GET",
        &format!("/v1/projects/{}", v["id"].as_str().unwrap()),
        &tok,
        None,
    )
    .await;
    assert_eq!(
        v["status"], "running",
        "the started status was not persisted"
    );
}

#[tokio::test]
async fn a_project_whose_sandbox_will_not_start_is_still_created() {
    // The row exists, so the create succeeded. Failing the request would leave the caller with a
    // project they never saw and cannot list, and no way to retry the start.
    let orch = FakeOrch {
        fail_start: Arc::new(Mutex::new(true)),
        ..Default::default()
    };
    let Some((app, u)) = app(orch).await else {
        return;
    };
    let tok = token(&u);

    let (s, v) = call(
        &app,
        "POST",
        "/v1/projects",
        &tok,
        Some(json!({"name": "dud"})),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    assert_eq!(v["status"], "error");

    let (s, list) = call(&app, "GET", "/v1/projects", &tok, None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(list.as_array().unwrap().len(), 1);
}

/// A full disk is the owner's to act on, so it must not read as "an unexpected error occurred".
///
/// This is the shape of a whole afternoon: the volume filled, sqlite reported it as an error about
/// shared memory, the host reported that as a failed start, and the API reported that as a 500 with
/// no cause. The host names the disk now; the API has to carry the name rather than flatten it.
#[tokio::test]
async fn a_start_refused_for_disk_says_so_instead_of_internal() {
    let orch = FakeOrch {
        out_of_disk: Arc::new(Mutex::new(true)),
        ..Default::default()
    };
    let Some((app, u)) = app(orch).await else {
        return;
    };
    let tok = token(&u);
    let id = project(&app, &tok).await;

    let (status, body) = call(
        &app,
        "POST",
        &format!("/v1/projects/{id}/start"),
        &tok,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::INSUFFICIENT_STORAGE);
    assert_eq!(body["error"]["code"], "insufficient_storage");
    let message = body["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("no room"),
        "the owner has to be told what to do about it: {message}"
    );
}

/// And every other host failure still reads as internal, so the new case is a distinction rather
/// than a relabelling of everything that goes wrong upstream.
#[tokio::test]
async fn an_ordinary_host_failure_is_still_internal() {
    let orch = FakeOrch {
        fail_start: Arc::new(Mutex::new(true)),
        ..Default::default()
    };
    let Some((app, u)) = app(orch).await else {
        return;
    };
    let tok = token(&u);
    let id = project(&app, &tok).await;

    let (status, body) = call(
        &app,
        "POST",
        &format!("/v1/projects/{id}/start"),
        &tok,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(body["error"]["code"], "internal");
}
