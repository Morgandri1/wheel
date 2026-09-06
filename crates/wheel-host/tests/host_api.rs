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
    /// Vault keys the host handed down, to prove the engine gets a key it can decode.
    start_vault_keys: Vec<String>,
}

#[derive(Clone)]
struct FakeSandbox {
    calls: Arc<Mutex<Calls>>,
    status: Status,
    fail_start: bool,
    fail_stop: bool,
    fail_restart: bool,
    fail_destroy: bool,
    engine_base: String,
}

impl FakeSandbox {
    fn new() -> Self {
        Self {
            calls: Arc::new(Mutex::new(Calls::default())),
            status: Status::Running,
            fail_start: false,
            fail_stop: false,
            fail_restart: false,
            fail_destroy: false,
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
        c.start_vault_keys.push(s.vault_key.clone());
        Ok(())
    }
    async fn stop(&self, id: &Uuid) -> anyhow::Result<()> {
        self.calls.lock().unwrap().stopped.push(*id);
        if self.fail_stop {
            anyhow::bail!("stop failed");
        }
        Ok(())
    }
    async fn restart(&self, id: &Uuid, _: &Secrets) -> anyhow::Result<()> {
        self.calls.lock().unwrap().restarted.push(*id);
        if self.fail_restart {
            anyhow::bail!("restart failed");
        }
        Ok(())
    }
    async fn destroy(&self, id: &Uuid) -> anyhow::Result<()> {
        self.calls.lock().unwrap().destroyed.push(*id);
        if self.fail_destroy {
            anyhow::bail!("destroy failed");
        }
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
        reap_grace_secs: 1,
        disk_floor_mb: 1,
        engine_base_url: "http://127.0.0.1:1".into(),
    }
}

/// A router backed by a throwaway on-disk sqlite file.
fn harness(sandbox: FakeSandbox) -> (axum::Router, Arc<Mutex<Calls>>, Arc<Store>) {
    let (app, calls, store, _) = harness_at(sandbox);
    (app, calls, store)
}

/// Same, but also returns the database path, for tests that need to damage it.
fn harness_at(
    sandbox: FakeSandbox,
) -> (
    axum::Router,
    Arc<Mutex<Calls>>,
    Arc<Store>,
    std::path::PathBuf,
) {
    let path = std::env::temp_dir().join(format!("wheel-host-test-{}.db", Uuid::new_v4()));
    let store = Arc::new(Store::open(path.to_str().unwrap()).expect("open store"));
    let calls = sandbox.calls.clone();
    let state = HostState {
        cfg: test_config(),
        sandbox: Arc::new(sandbox),
        store: store.clone(),
        http: reqwest::Client::new(),
        auth_limiter: std::sync::Arc::new(wheel_host::auth_limit::AuthLimiter::new(30)),
        ready: wheel_host::Readiness::serving_from_start(),
    };
    (build_router(state), calls, store, path)
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

// ---------------------------------------------------------------- failure branches

/// A sandbox that cannot be destroyed must keep its record.
///
/// Deleting the row anyway would strand the sandbox: a project nothing has a record of is a
/// project nobody will ever clean up, and on the process backend that means a uid, a data
/// directory and possibly a live engine with no owner.
#[tokio::test]
async fn a_failed_destroy_keeps_the_record() {
    let mut sandbox = FakeSandbox::new();
    sandbox.fail_destroy = true;
    let (app, _calls, store) = harness(sandbox);
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
        "DELETE",
        &format!("/host/v1/projects/{id}"),
        Some(SECRET),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);

    assert!(
        store.get(&id).await.unwrap().is_some(),
        "the record was deleted even though its sandbox is still out there"
    );
}

/// A failing stop is reported, not swallowed.
#[tokio::test]
async fn a_failed_stop_is_an_error_but_still_clears_intent() {
    let mut sandbox = FakeSandbox::new();
    sandbox.fail_stop = true;
    let (app, _calls, store) = harness(sandbox);
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
    assert!(is_desired_running(&store, &id).await);

    let (status, _) = call(
        &app,
        "POST",
        &format!("/host/v1/projects/{id}/stop"),
        Some(SECRET),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);

    // Intent is cleared before the sandbox is touched, deliberately: the operator asked for it to
    // be stopped, so a reboot must not bring it back just because the stop itself failed.
    assert!(
        !is_desired_running(&store, &id).await,
        "a failed stop left the project marked desired-running; a reboot would resurrect it"
    );
}

#[tokio::test]
async fn a_failed_restart_is_reported() {
    let mut sandbox = FakeSandbox::new();
    sandbox.fail_restart = true;
    let (app, _calls, _store) = harness(sandbox);
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
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
}

/// Deleting something that was never provisioned succeeds.
///
/// Delete has to converge. The API calls it during project teardown, and failing because the
/// sandbox is already gone would leave the caller unable to finish removing the project.
#[tokio::test]
async fn deleting_an_unknown_project_still_succeeds() {
    let (app, calls, _store) = harness(FakeSandbox::new());
    let unknown = Uuid::new_v4();

    let (status, _) = call(
        &app,
        "DELETE",
        &format!("/host/v1/projects/{unknown}"),
        Some(SECRET),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(
        calls.lock().unwrap().destroyed.contains(&unknown),
        "destroy should still be attempted for an unknown record, in case a sandbox exists"
    );
}

/// healthz names the backend, and needs the bearer like everything else.
#[tokio::test]
async fn healthz_reports_the_backend_in_use() {
    let (app, _calls, _store) = harness(FakeSandbox::new());
    let (status, body) = call(&app, "GET", "/host/v1/healthz", Some(SECRET), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ok"], true);
    assert_eq!(
        body["sandbox_backend"], "docker",
        "the operator needs to know which backend answered, not just that something did"
    );
}

/// The one deliberate exception to "every route requires the bearer".
///
/// A platform health checker cannot present the host secret. With the whole router behind the
/// bearer, every check answered 401, Railway stopped the container, and every project create hung
/// on an unreachable host — an outage caused by the health check that was supposed to prevent one.
#[tokio::test]
async fn liveness_answers_without_a_bearer_and_describes_nothing() {
    let (app, _, _) = harness(FakeSandbox::new());

    let (status, body) = call(&app, "GET", "/healthz", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ok"], serde_json::json!(true));

    // Liveness is not an information endpoint: anything that describes the host stays behind the
    // bearer, so an unauthenticated caller learns only that a process answered.
    let rendered = body.to_string();
    for leak in ["sandbox_backend", "process", "docker", "projects"] {
        assert!(
            !rendered.contains(leak),
            "unauthenticated liveness disclosed {leak}: {rendered}"
        );
    }
}

/// The detailed health endpoint keeps the bearer; only `/healthz` is exempt.
#[tokio::test]
async fn the_detailed_health_endpoint_is_still_bearer_gated() {
    let (app, _, _) = harness(FakeSandbox::new());
    let (status, _) = call(&app, "GET", "/host/v1/healthz", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

/// A vault key stored in the wrong base64 alphabet must be corrected *in host.db*, not only on the
/// path the API happens to take.
///
/// The engine decodes WHEEL_VAULT_KEY with the standard alphabet. Keys were minted URL-safe, so
/// every engine reconciled at host boot logged "WHEEL_VAULT_KEY is unusable" and served no vaults —
/// a fix applied only when a project is started through the API never reaches those.
#[tokio::test]
async fn a_vault_key_the_engine_cannot_decode_is_corrected_when_it_is_stored() {
    use base64::Engine as _;
    let bytes = [0xfbu8; 32]; // encodes differently in the two alphabets
    let url_safe = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
    let standard = base64::engine::general_purpose::STANDARD.encode(bytes);
    assert_ne!(
        url_safe, standard,
        "the test key must distinguish alphabets"
    );

    let (app, calls, store) = harness(FakeSandbox::new());
    let id = Uuid::new_v4();
    let (status, _) = call(
        &app,
        "PUT",
        &format!("/host/v1/projects/{id}"),
        Some(SECRET),
        Some(serde_json::json!({"engine_secret": "s", "vault_key": url_safe})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let stored = store.get(&id).await.unwrap().expect("record");
    assert_eq!(
        stored.vault_key, standard,
        "host.db must hold the key in the form the engine decodes"
    );
    assert_eq!(
        calls.lock().unwrap().provisioned,
        vec![id],
        "the sandbox is still provisioned"
    );
}

/// Rows written before canonicalisation existed are rewritten on boot, which is the only moment the
/// host looks at every project.
#[tokio::test]
async fn boot_rewrites_a_stored_vault_key_the_engine_could_not_decode() {
    use base64::Engine as _;
    let bytes = [0xfbu8; 32];
    let url_safe = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
    let standard = base64::engine::general_purpose::STANDARD.encode(bytes);

    let path = std::env::temp_dir().join(format!("wheel-host-test-{}.db", Uuid::new_v4()));
    let store = Arc::new(Store::open(path.to_str().unwrap()).expect("open store"));
    let id = Uuid::new_v4();
    // Exactly what an older host left behind: the unusable spelling, marked as running.
    store.upsert(&id, "engine-secret", &url_safe).await.unwrap();
    store.set_desired_running(&id, true).await.unwrap();

    let sandbox = FakeSandbox::new();
    let calls = sandbox.calls.clone();
    let state = HostState {
        cfg: test_config(),
        sandbox: Arc::new(sandbox),
        store: store.clone(),
        http: reqwest::Client::new(),
        auth_limiter: Arc::new(wheel_host::auth_limit::AuthLimiter::new(30)),
        ready: wheel_host::Readiness::serving_from_start(),
    };
    wheel_host::reconcile_on_boot(&state).await;

    assert_eq!(
        calls.lock().unwrap().start_vault_keys,
        vec![standard.clone()],
        "the reconciled engine must be started with a decodable key"
    );
    assert_eq!(
        store.get(&id).await.unwrap().unwrap().vault_key,
        standard,
        "and the correction must be persisted, so the next boot is already clean"
    );
}

/// The boot path, driven over a real socket.
///
/// `serve_on` is what the binary runs; before this it had no test at all, so the wiring that only
/// executes in production — connect info for the bearer limiter, the router assembled for real —
/// was the one part of the host nothing exercised.
#[tokio::test]
async fn the_server_boots_and_serves_the_real_router() {
    let (_, _, store) = harness(FakeSandbox::new());
    let state = HostState {
        cfg: test_config(),
        sandbox: Arc::new(FakeSandbox::new()),
        store,
        http: reqwest::Client::new(),
        auth_limiter: Arc::new(wheel_host::auth_limit::AuthLimiter::new(30)),
        ready: wheel_host::Readiness::serving_from_start(),
    };

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { wheel_host::serve_on(listener, state).await });

    let http = reqwest::Client::new();
    let live = http
        .get(format!("http://{addr}/healthz"))
        .send()
        .await
        .expect("liveness over a real socket");
    assert_eq!(live.status(), 200);
    assert_eq!(live.json::<serde_json::Value>().await.unwrap()["ok"], true);

    // The bearer really is enforced by the served router, not only by the in-process one.
    let denied = http
        .get(format!("http://{addr}/host/v1/healthz"))
        .send()
        .await
        .unwrap();
    assert_eq!(denied.status(), 401);

    let allowed = http
        .get(format!("http://{addr}/host/v1/healthz"))
        .bearer_auth(SECRET)
        .send()
        .await
        .unwrap();
    assert_eq!(allowed.status(), 200);

    server.abort();
}

/// Break the host's own database out from under it.
///
/// Dropping the table is the closest reachable stand-in for the real thing — a corrupt file, a full
/// or unmounted volume — and it makes every store call fail at once.
fn break_the_database(path: &std::path::Path) {
    let conn = rusqlite::Connection::open(path).expect("open the store directly");
    conn.execute_batch("DROP TABLE projects;")
        .expect("drop the table");
}

/// Every route must answer with our error envelope when the store is broken, not panic and not
/// report success. A host whose database is gone still has to say so in a way the API can relay.
#[tokio::test]
async fn a_broken_database_is_reported_on_every_route_rather_than_panicking() {
    let (app, _, _, path) = harness_at(FakeSandbox::new());
    let id = Uuid::new_v4();
    break_the_database(&path);

    let cases: Vec<(&str, String, Option<serde_json::Value>)> = vec![
        (
            "PUT",
            format!("/host/v1/projects/{id}"),
            Some(serde_json::json!({"engine_secret": "s", "vault_key": "k"})),
        ),
        ("GET", format!("/host/v1/projects/{id}"), None),
        ("POST", format!("/host/v1/projects/{id}/start"), None),
        ("POST", format!("/host/v1/projects/{id}/stop"), None),
        ("POST", format!("/host/v1/projects/{id}/restart"), None),
        ("DELETE", format!("/host/v1/projects/{id}"), None),
    ];

    for (method, path, body) in cases {
        let (status, body) = call(&app, method, &path, Some(SECRET), body).await;
        assert_eq!(
            status,
            StatusCode::INTERNAL_SERVER_ERROR,
            "{method} {path} did not report the broken store"
        );
        assert!(
            body["error"]["code"].is_string(),
            "{method} {path} answered without an error envelope: {body}"
        );
        // The message must not carry sqlite's own text to the caller: it is the API's job to relay
        // this, and a storage detail is not something a tenant should read.
        let message = body["error"]["message"].as_str().unwrap_or_default();
        assert!(
            !message.contains("no such table"),
            "{method} {path} leaked the storage error: {message}"
        );
    }
}

/// Reconcile has to survive a database it cannot read: the host still needs to come up and serve,
/// so an operator can see what is wrong instead of finding a process that exited at boot.
#[tokio::test]
async fn reconcile_survives_a_database_it_cannot_read() {
    let path = std::env::temp_dir().join(format!("wheel-host-test-{}.db", Uuid::new_v4()));
    let store = Arc::new(Store::open(path.to_str().unwrap()).expect("open store"));
    let sandbox = FakeSandbox::new();
    let calls = sandbox.calls.clone();
    let state = HostState {
        cfg: test_config(),
        sandbox: Arc::new(sandbox),
        store,
        http: reqwest::Client::new(),
        auth_limiter: Arc::new(wheel_host::auth_limit::AuthLimiter::new(30)),
        ready: wheel_host::Readiness::serving_from_start(),
    };
    break_the_database(&path);

    wheel_host::reconcile_on_boot(&state).await;

    assert!(
        calls.lock().unwrap().started.is_empty(),
        "nothing can be restored from a database that cannot be read"
    );
}

/// Liveness answers before reconcile finishes; project routes do not.
///
/// Both halves are the outage. The host bound its port only after restoring every project, which
/// takes longer than the platform's 30s health-check window, so the replica was declared unhealthy
/// and stopped — and a host that is stopped for failing a health check never reconciles at all.
/// Serving project routes early would have been the opposite mistake: reporting projects as
/// stopped that are in fact running, which the API relays to the user as fact.
#[tokio::test]
async fn liveness_answers_while_still_starting_but_project_routes_do_not() {
    let (_, _, store) = harness(FakeSandbox::new());
    let state = HostState {
        cfg: test_config(),
        sandbox: Arc::new(FakeSandbox::new()),
        store,
        http: reqwest::Client::new(),
        auth_limiter: Arc::new(wheel_host::auth_limit::AuthLimiter::new(30)),
        ready: wheel_host::Readiness::serving_after_reconcile(),
    };
    let ready = state.ready.clone();
    let app = wheel_host::build_router(state);

    let (status, body) = call(&app, "GET", "/healthz", None, None).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "liveness must not wait for reconcile"
    );
    assert_eq!(body["ok"], serde_json::json!(true));

    let id = Uuid::new_v4();
    let (status, body) = call(
        &app,
        "GET",
        &format!("/host/v1/projects/{id}"),
        Some(SECRET),
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "a project route answered from a half-restored view"
    );
    assert_eq!(body["error"]["code"], serde_json::json!("starting"));

    // And they open once reconcile is done.
    ready.open();
    let (status, _) = call(
        &app,
        "GET",
        &format!("/host/v1/projects/{id}"),
        Some(SECRET),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "unknown project, but served");
}

/// Starting-up must not be observable without the bearer: the ready gate sits inside the bearer
/// layer, so an unauthenticated caller gets 401 either way.
#[tokio::test]
async fn starting_up_is_not_visible_to_an_unauthenticated_caller() {
    let (_, _, store) = harness(FakeSandbox::new());
    let state = HostState {
        cfg: test_config(),
        sandbox: Arc::new(FakeSandbox::new()),
        store,
        http: reqwest::Client::new(),
        auth_limiter: Arc::new(wheel_host::auth_limit::AuthLimiter::new(30)),
        ready: wheel_host::Readiness::serving_after_reconcile(),
    };
    let app = wheel_host::build_router(state);
    let (status, _) = call(
        &app,
        "GET",
        &format!("/host/v1/projects/{}", Uuid::new_v4()),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

/// A full volume must refuse the start, not spawn a sandbox that will corrupt its own database.
///
/// This is the ninety-minute outage in one test. `/data` hit 100%, sqlite reported it as "disk I/O
/// error ... trying to resize an existing shared-memory segment" while growing a WAL index, and two
/// of us spent the afternoon on journal modes because the number nobody had was `df`. A refusal
/// that names the disk would have ended it in a minute.
#[tokio::test]
async fn a_full_volume_refuses_the_start_and_says_it_is_the_disk() {
    let path = std::env::temp_dir().join(format!("wheel-host-test-{}.db", Uuid::new_v4()));
    let store = Arc::new(Store::open(path.to_str().unwrap()).expect("open store"));
    let sandbox = FakeSandbox::new();
    let calls = sandbox.calls.clone();
    let state = HostState {
        // A floor no machine can meet is how a full disk is simulated without filling one.
        cfg: Config {
            disk_floor_mb: u64::MAX / (1024 * 1024),
            ..test_config()
        },
        sandbox: Arc::new(sandbox),
        store,
        http: reqwest::Client::new(),
        auth_limiter: std::sync::Arc::new(wheel_host::auth_limit::AuthLimiter::new(30)),
        ready: wheel_host::Readiness::serving_from_start(),
    };
    let app = build_router(state);

    let id = Uuid::new_v4();
    let (status, _) = call(
        &app,
        "PUT",
        &format!("/host/v1/projects/{id}"),
        Some(SECRET),
        Some(serde_json::json!({"engine_secret": "s", "vault_key": "dg=="})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = call(
        &app,
        "POST",
        &format!("/host/v1/projects/{id}/start"),
        Some(SECRET),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::INSUFFICIENT_STORAGE);
    let reason = body["last_error"].as_str().unwrap_or_default();
    assert!(
        reason.contains("the volume is full"),
        "the refusal has to name the disk, not a symptom: {reason}"
    );
    assert!(
        calls.lock().unwrap().started.is_empty(),
        "a sandbox was started onto a volume with no room in it"
    );
}

/// healthz carries the number that was missing, so the next person does not have to ssh for it.
#[tokio::test]
async fn healthz_reports_how_full_the_volume_is() {
    let (app, _calls, _store) = harness(FakeSandbox::new());
    let (status, body) = call(&app, "GET", "/host/v1/healthz", Some(SECRET), None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body["disk_free_mb"].as_u64().is_some(),
        "no free-space figure on healthz: {body}"
    );
    let used = body["disk_used_percent"].as_u64().expect("used percent");
    assert!(used <= 100, "{used}% used");
}
