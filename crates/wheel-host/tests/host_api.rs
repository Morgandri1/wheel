//! Host API tests.
//!
//! The host is the most privileged process in the system: it holds every project's engine secret,
//! performs the per-child setuid, and is the only thing that touches a container runtime. It had
//! no tests at all, which is the wrong place in this codebase to have none.
//!
//! These drive the real router — bearer middleware included — against a `FakeSandbox`, so the
//! lifecycle and authorisation logic is exercised without needing a container runtime. Docker's
//! own lifecycle is proven separately in the e2e run.

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use std::sync::{Arc, Mutex};
use tower::ServiceExt;
use uuid::Uuid;
use wheel_host::config::{Backend, Config};
use wheel_host::sandbox::{Sandbox, Secrets, Status};
use wheel_host::{build_router, store::Store, HostState};

const SECRET: &str = "test-host-secret-at-least-16";

/// Records what the host asked the sandbox to do, so tests can assert on the orchestration itself
/// rather than only on status codes.
#[derive(Default)]
struct Calls {
    provisioned: Vec<Uuid>,
    started: Vec<Uuid>,
    stopped: Vec<Uuid>,
    destroyed: Vec<Uuid>,
    restarted: Vec<Uuid>,
    /// Secrets the host handed down. Used to prove the host passes through what it stored.
    start_secrets: Vec<String>,
}

#[derive(Clone)]
struct FakeSandbox {
    calls: Arc<Mutex<Calls>>,
    status: Status,
    fail_start: bool,
    engine_base: String,
}

impl FakeSandbox {
    fn new() -> Self {
        Self {
            calls: Arc::new(Mutex::new(Calls::default())),
            status: Status::Running,
            fail_start: false,
            engine_base: "http://127.0.0.1:1".into(),
        }
    }
}

#[async_trait]
impl Sandbox for FakeSandbox {
    async fn provision(&self, id: &Uuid, _: &Secrets) -> anyhow::Result<()> {
        self.calls.lock().unwrap().provisioned.push(*id);
        Ok(())
    }
    async fn start(&self, id: &Uuid, s: &Secrets) -> anyhow::Result<()> {
        if self.fail_start {
            anyhow::bail!("engine did not become healthy");
        }
        let mut c = self.calls.lock().unwrap();
        c.started.push(*id);
        c.start_secrets.push(s.engine_secret.clone());
        Ok(())
    }
    async fn stop(&self, id: &Uuid) -> anyhow::Result<()> {
        self.calls.lock().unwrap().stopped.push(*id);
        Ok(())
    }
    async fn restart(&self, id: &Uuid, _: &Secrets) -> anyhow::Result<()> {
        self.calls.lock().unwrap().restarted.push(*id);
        Ok(())
    }
    async fn destroy(&self, id: &Uuid) -> anyhow::Result<()> {
        self.calls.lock().unwrap().destroyed.push(*id);
        Ok(())
    }
    async fn status(&self, _: &Uuid) -> anyhow::Result<Status> {
        Ok(self.status)
    }
    fn engine_base(&self, _: &Uuid) -> String {
        self.engine_base.clone()
    }
}

fn test_config() -> Config {
    Config {
        bind_addr: "127.0.0.1:0".into(),
        secret: SECRET.into(),
        backend: Backend::Docker,
        data_dir: "/tmp".into(),
        engine_image: "wheel-engine:stub".into(),
        docker_network: "wheel".into(),
        engine_port: 7000,
        memory_bytes: 1024 * 1024 * 1024,
        nano_cpus: 1_000_000_000,
        pids_limit: 512,
        start_timeout_secs: 30,
        uid_range_start: 20_000,
        uid_stride: 64,
        run_dir: "/tmp/wheel-run-test".into(),
        rlimit_nproc: 4096,
        rlimit_address_space_bytes: None,
        rlimit_fsize_bytes: 8 * 1024 * 1024 * 1024,
        rlimit_nofile: 16384,
        rlimit_cpu_secs: None,
        engine_base_url: "http://127.0.0.1:1".into(),
    }
}

/// A router backed by a throwaway on-disk sqlite file.
fn harness(sandbox: FakeSandbox) -> (axum::Router, Arc<Mutex<Calls>>, Arc<Store>) {
    let path = std::env::temp_dir().join(format!("wheel-host-test-{}.db", Uuid::new_v4()));
    let store = Arc::new(Store::open(path.to_str().unwrap()).expect("open store"));
    let calls = sandbox.calls.clone();
    let state = HostState {
        cfg: test_config(),
        sandbox: Arc::new(sandbox),
        store: store.clone(),
        http: reqwest::Client::new(),
        auth_limiter: std::sync::Arc::new(wheel_host::auth_limit::AuthLimiter::new(30)),
    };
    (build_router(state), calls, store)
}

async fn call(
    app: &axum::Router,
    method: &str,
    path: &str,
    bearer: Option<&str>,
    body: Option<serde_json::Value>,
) -> (StatusCode, serde_json::Value) {
    let mut req = Request::builder().method(method).uri(path);
    if let Some(b) = bearer {
        req = req.header("authorization", format!("Bearer {b}"));
    }
    let req = match body {
        Some(v) => req
            .header("content-type", "application/json")
            .body(Body::from(v.to_string()))
            .unwrap(),
        None => req.body(Body::empty()).unwrap(),
    };
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

/// `desired_running` is a query predicate rather than a column on the record, so membership in the
/// restore-on-boot set is the thing to assert.
async fn is_desired_running(store: &Store, id: &Uuid) -> bool {
    store
        .all_desired_running()
        .await
        .unwrap()
        .iter()
        .any(|r| &r.id == id)
}

fn secrets() -> serde_json::Value {
    serde_json::json!({ "engine_secret": "engine-secret-abc", "vault_key": "vault-key-xyz" })
}

// --- the bearer boundary -----------------------------------------------------------------------
//
// Anything that reaches this port can control every tenant's sandbox, so the bearer is the whole
// perimeter. It must cover *every* route, not just the interesting ones.

#[tokio::test]
async fn every_route_requires_the_bearer() {
    let (app, _, _) = harness(FakeSandbox::new());
    let id = Uuid::new_v4();

    let routes = [
        ("GET", "/host/v1/healthz".to_string()),
        ("PUT", format!("/host/v1/projects/{id}")),
        ("GET", format!("/host/v1/projects/{id}")),
        ("DELETE", format!("/host/v1/projects/{id}")),
        ("POST", format!("/host/v1/projects/{id}/start")),
        ("POST", format!("/host/v1/projects/{id}/stop")),
        ("POST", format!("/host/v1/projects/{id}/restart")),
        ("GET", format!("/host/v1/projects/{id}/engine/v1/board")),
        ("GET", format!("/host/v1/projects/{id}/ingress/hook")),
    ];

    for (method, path) in routes {
        let (status, _) = call(&app, method, &path, None, None).await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "{method} {path} served without a bearer"
        );

        let (status, _) = call(&app, method, &path, Some("wrong-secret"), None).await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "{method} {path} accepted a wrong bearer"
        );
    }
}

#[tokio::test]
async fn bearer_must_match_exactly() {
    let (app, _, _) = harness(FakeSandbox::new());
    // Prefixes and extensions of the real secret must not pass: a length-insensitive or
    // prefix-matching comparison would be a serious weakness.
    for bad in [
        &SECRET[..SECRET.len() - 1],
        "",
        "   ",
        &format!("{SECRET}x"),
    ] {
        let (status, _) = call(&app, "GET", "/host/v1/healthz", Some(bad), None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "accepted bearer {bad:?}");
    }
    let (status, body) = call(&app, "GET", "/host/v1/healthz", Some(SECRET), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ok"], true);
    assert_eq!(body["sandbox_backend"], "docker");
}

// --- lifecycle ---------------------------------------------------------------------------------

#[tokio::test]
async fn put_provisions_and_is_idempotent() {
    let (app, calls, store) = harness(FakeSandbox::new());
    let id = Uuid::new_v4();

    for _ in 0..2 {
        let (status, _) = call(
            &app,
            "PUT",
            &format!("/host/v1/projects/{id}"),
            Some(SECRET),
            Some(secrets()),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }

    assert_eq!(
        calls.lock().unwrap().provisioned.len(),
        2,
        "provision should be callable twice"
    );
    let rec = store.get(&id).await.unwrap().expect("record stored");
    assert_eq!(rec.engine_secret, "engine-secret-abc");
    assert!(
        !is_desired_running(&store, &id).await,
        "a provisioned project is not yet marked as one to restore on boot"
    );
}

#[tokio::test]
async fn start_records_intent_and_passes_the_stored_secret() {
    let (app, calls, store) = harness(FakeSandbox::new());
    let id = Uuid::new_v4();
    call(
        &app,
        "PUT",
        &format!("/host/v1/projects/{id}"),
        Some(SECRET),
        Some(secrets()),
    )
    .await;

    let (status, body) = call(
        &app,
        "POST",
        &format!("/host/v1/projects/{id}/start"),
        Some(SECRET),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "running");

    // The host must hand the engine the secret it stored — that is the whole point of it being the
    // only process holding them.
    assert_eq!(
        calls.lock().unwrap().start_secrets,
        vec!["engine-secret-abc".to_string()]
    );
    assert!(is_desired_running(&store, &id).await);
}

#[tokio::test]
async fn a_failed_start_does_not_record_running_intent() {
    // Otherwise every future boot would try to resurrect a project that cannot start.
    let mut sandbox = FakeSandbox::new();
    sandbox.fail_start = true;
    let (app, _, store) = harness(sandbox);
    let id = Uuid::new_v4();
    call(
        &app,
        "PUT",
        &format!("/host/v1/projects/{id}"),
        Some(SECRET),
        Some(secrets()),
    )
    .await;

    let (status, body) = call(
        &app,
        "POST",
        &format!("/host/v1/projects/{id}/start"),
        Some(SECRET),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::GATEWAY_TIMEOUT);
    assert_eq!(body["status"], "error");
    assert!(
        !is_desired_running(&store, &id).await,
        "a failed start was recorded as desired-running; every boot would retry it forever"
    );
}

#[tokio::test]
async fn stop_clears_intent_and_delete_removes_everything() {
    let (app, calls, store) = harness(FakeSandbox::new());
    let id = Uuid::new_v4();
    call(
        &app,
        "PUT",
        &format!("/host/v1/projects/{id}"),
        Some(SECRET),
        Some(secrets()),
    )
    .await;
    call(
        &app,
        "POST",
        &format!("/host/v1/projects/{id}/start"),
        Some(SECRET),
        None,
    )
    .await;

    let (status, _) = call(
        &app,
        "POST",
        &format!("/host/v1/projects/{id}/stop"),
        Some(SECRET),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(!is_desired_running(&store, &id).await);

    let (status, _) = call(
        &app,
        "DELETE",
        &format!("/host/v1/projects/{id}"),
        Some(SECRET),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(
        store.get(&id).await.unwrap().is_none(),
        "record survived delete"
    );
    let c = calls.lock().unwrap();
    assert_eq!(c.stopped.len(), 1);
    assert_eq!(c.destroyed.len(), 1);
}

#[tokio::test]
async fn restart_requires_a_known_project() {
    let (app, calls, _) = harness(FakeSandbox::new());
    let unknown = Uuid::new_v4();

    for path in [
        format!("/host/v1/projects/{unknown}/start"),
        format!("/host/v1/projects/{unknown}/restart"),
    ] {
        let (status, _) = call(&app, "POST", &path, Some(SECRET), None).await;
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "{path} acted on an unknown project"
        );
    }
    let c = calls.lock().unwrap();
    assert!(
        c.started.is_empty() && c.restarted.is_empty(),
        "sandbox was touched for an unknown project"
    );
}

#[tokio::test]
async fn get_reports_status_for_a_known_project_only() {
    let (app, _, _) = harness(FakeSandbox::new());
    let id = Uuid::new_v4();

    let (status, _) = call(
        &app,
        "GET",
        &format!("/host/v1/projects/{id}"),
        Some(SECRET),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    call(
        &app,
        "PUT",
        &format!("/host/v1/projects/{id}"),
        Some(SECRET),
        Some(secrets()),
    )
    .await;
    let (status, body) = call(
        &app,
        "GET",
        &format!("/host/v1/projects/{id}"),
        Some(SECRET),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "running");
}

#[tokio::test]
async fn restart_round_trips() {
    let (app, calls, _) = harness(FakeSandbox::new());
    let id = Uuid::new_v4();
    call(
        &app,
        "PUT",
        &format!("/host/v1/projects/{id}"),
        Some(SECRET),
        Some(secrets()),
    )
    .await;
    let (status, _) = call(
        &app,
        "POST",
        &format!("/host/v1/projects/{id}/restart"),
        Some(SECRET),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(calls.lock().unwrap().restarted, vec![id]);
}
