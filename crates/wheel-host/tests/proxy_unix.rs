//! Proxying to an engine that listens on a unix socket.
//!
//! This is the Railway path. In process mode there is no TCP endpoint to address — deliberately,
//! because on a shared kernel every loopback port is reachable by every other tenant — so the host
//! has to dial the socket directly. The code for that existed before this test did, which is
//! exactly why it needed one: an unexercised proxy is a proxy nobody has watched carry a byte.

use axum::body::Body;
use axum::extract::State;
use axum::http::{Request, StatusCode};
use axum::routing::any;
use axum::Router;
use std::sync::{Arc, Mutex};
use tower::ServiceExt;
use uuid::Uuid;
use wheel_host::config::{Backend, Config};
use wheel_host::sandbox::{Sandbox, Secrets, Status};
use wheel_host::{build_router, store::Store, HostState};

const HOST_SECRET: &str = "host-secret-at-least-16-chars";
const ENGINE_SECRET: &str = "engine-secret-for-this-project";

#[derive(Clone, Default)]
struct Seen {
    auth: Arc<Mutex<Option<String>>>,
    path: Arc<Mutex<Option<String>>>,
    method: Arc<Mutex<Option<String>>>,
    body: Arc<Mutex<Option<Vec<u8>>>>,
}

/// An engine listening on a unix socket, recording what it was sent.
async fn engine_on_socket(path: std::path::PathBuf) -> Seen {
    let seen = Seen::default();
    let app = Router::new()
        .fallback(any(
            |State(s): State<Seen>, req: Request<Body>| async move {
                *s.path.lock().unwrap() = Some(req.uri().path().to_string());
                *s.method.lock().unwrap() = Some(req.method().to_string());
                *s.auth.lock().unwrap() = req
                    .headers()
                    .get("authorization")
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string);
                let b = axum::body::to_bytes(req.into_body(), 1 << 20)
                    .await
                    .unwrap();
                *s.body.lock().unwrap() = Some(b.to_vec());
                axum::Json(serde_json::json!({"nodes": []}))
            },
        ))
        .with_state(seen.clone());

    let _ = std::fs::remove_file(&path);
    let listener = tokio::net::UnixListener::bind(&path).expect("bind unix socket");
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    // Give the accept loop a moment before the first dial.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    seen
}

/// A sandbox that reports a unix socket, exactly as the process backend does.
struct SocketSandbox(String);

#[async_trait::async_trait]
impl Sandbox for SocketSandbox {
    async fn provision(&self, _: &Uuid, _: &Secrets) -> anyhow::Result<()> {
        Ok(())
    }
    async fn start(&self, _: &Uuid, _: &Secrets) -> anyhow::Result<()> {
        Ok(())
    }
    async fn stop(&self, _: &Uuid) -> anyhow::Result<()> {
        Ok(())
    }
    async fn restart(&self, _: &Uuid, _: &Secrets) -> anyhow::Result<()> {
        Ok(())
    }
    async fn destroy(&self, _: &Uuid) -> anyhow::Result<()> {
        Ok(())
    }
    async fn status(&self, _: &Uuid) -> anyhow::Result<Status> {
        Ok(Status::Running)
    }
    fn engine_base(&self, _: &Uuid) -> String {
        self.0.clone()
    }
}

fn cfg() -> Config {
    Config {
        bind_addr: "127.0.0.1:0".into(),
        secret: HOST_SECRET.into(),
        backend: Backend::Process,
        data_dir: "/tmp".into(),
        engine_image: "unused".into(),
        docker_network: "unused".into(),
        engine_port: 7000,
        memory_bytes: 1 << 30,
        nano_cpus: 1_000_000_000,
        pids_limit: 512,
        start_timeout_secs: 5,
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
        reconcile_concurrency: 8,
        engine_base_url: "unused".into(),
    }
}

async fn harness() -> (Router, Uuid, Seen, std::path::PathBuf) {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();
    // Deliberately short: a unix socket path must fit in sockaddr_un.sun_path (~104 bytes), and
    // the platform temp directory is long enough on macOS to exceed it on its own.
    let dir = std::path::PathBuf::from(format!(
        "/tmp/wp{}",
        &Uuid::new_v4().simple().to_string()[..8]
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let socket = dir.join("e.sock");
    let seen = engine_on_socket(socket.clone()).await;

    let store = Arc::new(Store::open(dir.join("host.db").to_str().unwrap()).unwrap());
    let id = Uuid::new_v4();
    store.upsert(&id, ENGINE_SECRET, "vault-key").await.unwrap();

    let state = HostState {
        cfg: cfg(),
        sandbox: Arc::new(SocketSandbox(format!("unix://{}", socket.display()))),
        store,
        http: reqwest::Client::new(),
        auth_limiter: Arc::new(wheel_host::auth_limit::AuthLimiter::new(1_000)),
        ready: wheel_host::Readiness::serving_from_start(),
    };
    (build_router(state), id, seen, socket)
}

async fn send(app: &Router, method: &str, path: &str, body: Option<&str>) -> (StatusCode, String) {
    let b = Request::builder()
        .method(method)
        .uri(path)
        .header("authorization", format!("Bearer {HOST_SECRET}"));
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
    (status, String::from_utf8_lossy(&bytes).to_string())
}

#[tokio::test]
async fn proxies_to_an_engine_on_a_unix_socket() {
    let (app, id, seen, _s) = harness().await;
    let (status, body) = send(
        &app,
        "GET",
        &format!("/host/v1/projects/{id}/engine/v1/board"),
        None,
    )
    .await;

    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert!(
        body.contains("nodes"),
        "engine response did not come back: {body}"
    );
    assert_eq!(seen.path.lock().unwrap().clone().unwrap(), "/v1/board");
}

#[tokio::test]
async fn the_engine_secret_is_injected_over_the_socket_too() {
    // The credential swap has to hold on this path as much as on the TCP one: the API's host
    // secret must never reach a tenant's engine.
    let (app, id, seen, _s) = harness().await;
    send(
        &app,
        "GET",
        &format!("/host/v1/projects/{id}/engine/v1/board"),
        None,
    )
    .await;

    let auth = seen
        .auth
        .lock()
        .unwrap()
        .clone()
        .expect("engine saw no authorization");
    assert_eq!(auth, format!("Bearer {ENGINE_SECRET}"));
    assert!(
        !auth.contains(HOST_SECRET),
        "the host secret leaked to the engine"
    );
}

#[tokio::test]
async fn method_body_and_query_survive_the_socket_hop() {
    let (app, id, seen, _s) = harness().await;
    let (status, _) = send(
        &app,
        "POST",
        &format!("/host/v1/projects/{id}/engine/v1/nodes?dry_run=1"),
        Some(r#"{"type":"ctx"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    assert_eq!(seen.method.lock().unwrap().clone().unwrap(), "POST");
    assert_eq!(seen.path.lock().unwrap().clone().unwrap(), "/v1/nodes");
    assert_eq!(
        seen.body.lock().unwrap().clone().unwrap(),
        br#"{"type":"ctx"}"#.to_vec()
    );
}

#[tokio::test]
async fn ingress_reaches_the_engine_over_the_socket() {
    let (app, id, seen, _s) = harness().await;
    let (status, _) = send(
        &app,
        "GET",
        &format!("/host/v1/projects/{id}/ingress/hook/abc"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        seen.path.lock().unwrap().clone().unwrap(),
        "/ingress/hook/abc"
    );
}

#[tokio::test]
async fn a_socket_with_nothing_listening_is_an_enveloped_502() {
    // The engine died, or never started. The client must get our error shape, not a bare string
    // and not a hang.
    let dir = std::path::PathBuf::from(format!(
        "/tmp/wpd{}",
        &Uuid::new_v4().simple().to_string()[..8]
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let store = Arc::new(Store::open(dir.join("host.db").to_str().unwrap()).unwrap());
    let id = Uuid::new_v4();
    store.upsert(&id, ENGINE_SECRET, "vault-key").await.unwrap();

    let state = HostState {
        cfg: cfg(),
        sandbox: Arc::new(SocketSandbox(format!(
            "unix://{}",
            dir.join("absent.sock").display()
        ))),
        store,
        http: reqwest::Client::new(),
        auth_limiter: Arc::new(wheel_host::auth_limit::AuthLimiter::new(1_000)),
        ready: wheel_host::Readiness::serving_from_start(),
    };
    let app = build_router(state);

    let (status, body) = send(
        &app,
        "GET",
        &format!("/host/v1/projects/{id}/engine/v1/board"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    let v: serde_json::Value = serde_json::from_str(&body).expect("body should be JSON");
    assert_eq!(v["error"]["code"], "engine_unreachable");
}

#[tokio::test]
async fn traversal_is_refused_before_the_socket_is_dialled() {
    let (app, id, seen, _s) = harness().await;
    let (status, _) = send(
        &app,
        "GET",
        &format!("/host/v1/projects/{id}/engine/v1/../../etc/passwd"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        seen.path.lock().unwrap().is_none(),
        "a traversal attempt reached the engine"
    );
}
