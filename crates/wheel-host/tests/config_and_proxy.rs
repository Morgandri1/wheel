//! Host configuration validation, and the proxy's credential swap.
//!
//! The proxy test is the important one. The host sits between two different trust domains: the API
//! authenticates to it with `WHEEL_HOST_SECRET`, and it authenticates to each engine with that
//! project's own secret. Leaking the former downstream would hand any tenant's engine the key to
//! every other tenant's sandbox, so the swap is a security boundary, not plumbing.

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, Request, StatusCode};
use axum::routing::get;
use axum::Router;
use std::sync::{Arc, Mutex};
use tower::ServiceExt;
use uuid::Uuid;
use wheel_host::config::{Backend, Config};
use wheel_host::sandbox::{Sandbox, Secrets, Status};
use wheel_host::{build_router, store::Store, HostState};

const HOST_SECRET: &str = "host-secret-at-least-16-chars";
const ENGINE_SECRET: &str = "engine-secret-for-this-project";

// ---------------------------------------------------------------- config

/// Environment variables are process-global, so every case runs in one test, in sequence.
#[test]
fn config_validation() {
    fn base() {
        std::env::set_var("WHEEL_HOST_SECRET", HOST_SECRET);
        std::env::remove_var("SANDBOX_BACKEND");
        std::env::remove_var("WHEEL_ENV");
        std::env::remove_var("ENGINE_BASE_URL");
    }

    base();
    let cfg = Config::from_env().expect("defaults should load");
    assert_eq!(
        cfg.backend,
        Backend::Docker,
        "docker is the default backend"
    );
    assert_eq!(cfg.engine_port, 7000);

    // The bearer is the only thing between this port and every tenant's sandbox.
    base();
    std::env::set_var("WHEEL_HOST_SECRET", "short");
    assert!(
        Config::from_env().is_err(),
        "a short host secret must be refused, not accepted with a warning"
    );

    base();
    std::env::remove_var("WHEEL_HOST_SECRET");
    assert!(
        Config::from_env().is_err(),
        "a missing host secret must refuse to boot"
    );

    base();
    std::env::set_var("SANDBOX_BACKEND", "process");
    assert_eq!(Config::from_env().unwrap().backend, Backend::Process);

    base();
    std::env::set_var("SANDBOX_BACKEND", "nonsense");
    assert!(
        Config::from_env().is_err(),
        "an unknown backend must refuse to boot"
    );

    // `external` performs no isolation at all, so it is dev-only by construction.
    base();
    std::env::set_var("SANDBOX_BACKEND", "external");
    assert!(
        Config::from_env().is_err(),
        "external backend outside dev would be a sandbox that is not a sandbox"
    );

    base();
    std::env::set_var("SANDBOX_BACKEND", "external");
    std::env::set_var("WHEEL_ENV", "dev");
    assert_eq!(Config::from_env().unwrap().backend, Backend::External);

    base();
    let cfg = Config::from_env().unwrap();
    let id = Uuid::new_v4();
    assert_eq!(cfg.container_name(&id), format!("wheel-p-{id}"));
    assert_eq!(cfg.volume_name(&id), format!("wheel-p-{id}-data"));
    assert!(cfg.engine_url(&id).ends_with(":7000"));
}

// ---------------------------------------------------------------- proxy

/// A sandbox that reports an engine living at a URL we control.
struct PointedSandbox(String);

#[async_trait::async_trait]
impl Sandbox for PointedSandbox {
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

#[derive(Clone, Default)]
struct Seen {
    auth: Arc<Mutex<Option<String>>>,
    path: Arc<Mutex<Option<String>>>,
}

/// A stand-in engine that records what the host sent it.
async fn mock_engine() -> (String, Seen) {
    let seen = Seen::default();
    let app = Router::new()
        .route(
            "/{*rest}",
            get(
                |State(s): State<Seen>, headers: HeaderMap, uri: axum::http::Uri| async move {
                    *s.auth.lock().unwrap() = headers
                        .get("authorization")
                        .and_then(|v| v.to_str().ok())
                        .map(str::to_string);
                    *s.path.lock().unwrap() = Some(uri.path().to_string());
                    axum::Json(serde_json::json!({"nodes": []}))
                },
            ),
        )
        .with_state(seen.clone());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), seen)
}

fn cfg() -> Config {
    Config {
        bind_addr: "127.0.0.1:0".into(),
        secret: HOST_SECRET.into(),
        backend: Backend::Docker,
        data_dir: "/tmp".into(),
        engine_image: "wheel-engine:stub".into(),
        docker_network: "wheel".into(),
        engine_port: 7000,
        memory_bytes: 1 << 30,
        nano_cpus: 1_000_000_000,
        pids_limit: 512,
        start_timeout_secs: 30,
        engine_base_url: "http://127.0.0.1:1".into(),
    }
}

async fn harness(engine_base: String) -> (Router, Uuid) {
    let path = std::env::temp_dir().join(format!("wheel-host-proxy-{}.db", Uuid::new_v4()));
    let store = Arc::new(Store::open(path.to_str().unwrap()).unwrap());
    let id = Uuid::new_v4();
    store.upsert(&id, ENGINE_SECRET, "vault-key").await.unwrap();

    let state = HostState {
        cfg: cfg(),
        sandbox: Arc::new(PointedSandbox(engine_base)),
        store,
        http: reqwest::Client::new(),
    };
    (build_router(state), id)
}

async fn send(app: &Router, path: &str) -> (StatusCode, String) {
    let req = Request::builder()
        .method("GET")
        .uri(path)
        .header("authorization", format!("Bearer {HOST_SECRET}"))
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).to_string())
}

#[tokio::test]
async fn proxy_swaps_the_host_bearer_for_the_project_engine_secret() {
    let (engine, seen) = mock_engine().await;
    let (app, id) = harness(engine).await;

    let (status, body) = send(&app, &format!("/host/v1/projects/{id}/engine/v1/board")).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    let auth = seen
        .auth
        .lock()
        .unwrap()
        .clone()
        .expect("engine saw no authorization header");
    assert_eq!(auth, format!("Bearer {ENGINE_SECRET}"));
    assert!(
        !auth.contains(HOST_SECRET),
        "the API's host secret must never be forwarded to a tenant's engine"
    );
}

#[tokio::test]
async fn engine_path_is_forwarded_verbatim() {
    // Re-prefixing `v1/` here once produced /v1/v1/board and a 404 from the engine.
    let (engine, seen) = mock_engine().await;
    let (app, id) = harness(engine).await;
    send(&app, &format!("/host/v1/projects/{id}/engine/v1/board")).await;
    assert_eq!(seen.path.lock().unwrap().clone().unwrap(), "/v1/board");
}

#[tokio::test]
async fn ingress_is_forwarded_under_the_ingress_prefix() {
    let (engine, seen) = mock_engine().await;
    let (app, id) = harness(engine).await;
    send(&app, &format!("/host/v1/projects/{id}/ingress/hook/abc")).await;
    assert_eq!(
        seen.path.lock().unwrap().clone().unwrap(),
        "/ingress/hook/abc"
    );
}

#[tokio::test]
async fn proxying_an_unknown_project_is_an_enveloped_404() {
    let (engine, _) = mock_engine().await;
    let (app, _) = harness(engine).await;
    let (status, body) = send(
        &app,
        &format!("/host/v1/projects/{}/engine/v1/board", Uuid::new_v4()),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let v: serde_json::Value = serde_json::from_str(&body).expect("body should be JSON");
    assert_eq!(v["error"]["code"], "not_found");
}

#[tokio::test]
async fn an_unreachable_engine_is_an_enveloped_502() {
    // Port 1 refuses immediately, standing in for an engine that has died.
    let (app, id) = harness("http://127.0.0.1:1".into()).await;
    let (status, body) = send(&app, &format!("/host/v1/projects/{id}/engine/v1/board")).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    let v: serde_json::Value =
        serde_json::from_str(&body).expect("body should be JSON, not a bare string");
    assert_eq!(v["error"]["code"], "engine_unreachable");
}

#[tokio::test]
async fn traversal_in_the_proxy_path_is_refused() {
    let (engine, seen) = mock_engine().await;
    let (app, id) = harness(engine).await;
    let (status, _) = send(
        &app,
        &format!("/host/v1/projects/{id}/engine/v1/../../secret"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        seen.path.lock().unwrap().is_none(),
        "a traversal attempt must never reach the engine"
    );
}

// ---------------------------------------------------------------- external sandbox

/// The dev-only backend that points at an engine somebody else started.
///
/// It isolates nothing, which is why `Config` refuses it outside dev. What it must still get right
/// is status: reporting `running` for an engine that is not there would make the API tell a user
/// their project is up when it is not.
mod external {
    use super::*;
    use wheel_host::sandbox::external::ExternalSandbox;

    #[tokio::test]
    async fn status_is_running_only_when_the_engine_answers_healthz() {
        let healthy = Router::new().route(
            "/healthz",
            get(|| async { axum::Json(serde_json::json!({"ok": true})) }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, healthy).await.unwrap() });

        let s = ExternalSandbox::new(format!("http://{addr}"));
        assert_eq!(s.status(&Uuid::new_v4()).await.unwrap(), Status::Running);
    }

    #[tokio::test]
    async fn an_engine_that_is_not_there_reads_as_stopped() {
        let s = ExternalSandbox::new("http://127.0.0.1:1".into());
        assert_eq!(s.status(&Uuid::new_v4()).await.unwrap(), Status::Stopped);
    }

    #[tokio::test]
    async fn lifecycle_calls_are_no_ops_and_engine_base_is_the_configured_url() {
        // Nothing here owns the engine's existence, so start/stop must succeed without pretending
        // to have done anything.
        let s = ExternalSandbox::new("http://127.0.0.1:7000/".into());
        let id = Uuid::new_v4();
        let secrets = Secrets {
            engine_secret: "s".into(),
            vault_key: "v".into(),
        };
        s.provision(&id, &secrets).await.unwrap();
        s.start(&id, &secrets).await.unwrap();
        s.restart(&id, &secrets).await.unwrap();
        s.stop(&id).await.unwrap();
        s.destroy(&id).await.unwrap();
        // The trailing slash is trimmed so callers can join paths without doubling it.
        assert_eq!(s.engine_base(&id), "http://127.0.0.1:7000");
    }

    #[test]
    fn secrets_debug_is_redacted() {
        let s = Secrets {
            engine_secret: "super-secret-engine-value".into(),
            vault_key: "super-secret-vault-value".into(),
        };
        let rendered = format!("{s:?}");
        assert!(!rendered.contains("super-secret-engine-value"));
        assert!(!rendered.contains("super-secret-vault-value"));
    }
}
