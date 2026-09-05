//! The host's proxy to a project's engine.
//!
//! This hop is where the engine secret is injected, and it is the last place a mistake stays
//! private: whatever this returns is relayed to the browser verbatim by the API. So the tests
//! pin down three things — the engine bearer goes down and the caller's own credential does not,
//! the path is forwarded exactly as given, and every failure carries the error envelope rather
//! than a bare string no client can branch on.

use async_trait::async_trait;
use axum::body::Body;
use axum::extract::{Request as AxumRequest, State};
use axum::http::{HeaderMap, Request, StatusCode};
use axum::routing::any;
use axum::Router;
use std::sync::{Arc, Mutex};
use tower::ServiceExt;
use uuid::Uuid;
use wheel_host::config::{Backend, Config};
use wheel_host::sandbox::{Sandbox, Secrets, Status};
use wheel_host::{build_router, store::Store, HostState};

const SECRET: &str = "host-secret-at-least-16-chars";
const ENGINE_SECRET: &str = "engine-secret-for-this-project";

#[derive(Clone, Default)]
struct EngineSeen {
    path: Arc<Mutex<Option<String>>>,
    headers: Arc<Mutex<Option<HeaderMap>>>,
}

/// A stand-in engine that records exactly what reached it.
async fn mock_engine() -> (String, EngineSeen) {
    let seen = EngineSeen::default();
    let app = Router::new()
        .fallback(any(
            |State(s): State<EngineSeen>, req: AxumRequest<Body>| async move {
                *s.path.lock().unwrap() = Some(req.uri().path().to_string());
                *s.headers.lock().unwrap() = Some(req.headers().clone());
                (StatusCode::OK, r#"{"nodes":[]}"#)
            },
        ))
        .with_state(seen.clone());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), seen)
}

struct PointedSandbox {
    base: String,
}

#[async_trait]
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
        self.base.clone()
    }
}

fn cfg() -> Config {
    Config {
        bind_addr: "127.0.0.1:0".into(),
        secret: SECRET.into(),
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

async fn harness(engine_base: &str) -> (Router, Uuid) {
    let path = std::env::temp_dir().join(format!("wheel-host-proxy-{}.db", Uuid::new_v4()));
    let store = Arc::new(Store::open(path.to_str().unwrap()).expect("open store"));
    let id = Uuid::new_v4();
    store
        .upsert(&id, ENGINE_SECRET, "vault-key")
        .await
        .expect("seed project");

    let state = HostState {
        cfg: cfg(),
        sandbox: Arc::new(PointedSandbox {
            base: engine_base.to_string(),
        }),
        store,
        http: reqwest::Client::new(),
    };
    (build_router(state), id)
}

async fn get(app: &Router, path: &str, extra: &[(&str, &str)]) -> (StatusCode, String) {
    let mut req = Request::builder()
        .method("GET")
        .uri(path)
        .header("authorization", format!("Bearer {SECRET}"));
    for (k, v) in extra {
        req = req.header(*k, *v);
    }
    let resp = app
        .clone()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let body = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&body).to_string())
}

#[tokio::test]
async fn engine_bearer_is_injected_and_the_callers_is_not_forwarded() {
    let (engine, seen) = mock_engine().await;
    let (app, id) = harness(&engine).await;

    let (status, _) = get(
        &app,
        &format!("/host/v1/projects/{id}/engine/v1/board"),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let headers = seen
        .headers
        .lock()
        .unwrap()
        .clone()
        .expect("engine saw a request");
    let auth = headers.get("authorization").unwrap().to_str().unwrap();
    assert_eq!(
        auth,
        format!("Bearer {ENGINE_SECRET}"),
        "the engine must be handed its own per-project secret"
    );
    assert_ne!(
        auth,
        format!("Bearer {SECRET}"),
        "the host's own bearer must never reach a tenant's engine — that key opens every sandbox"
    );
}

#[tokio::test]
async fn the_engine_path_is_forwarded_verbatim() {
    let (engine, seen) = mock_engine().await;
    let (app, id) = harness(&engine).await;

    get(
        &app,
        &format!("/host/v1/projects/{id}/engine/v1/board"),
        &[],
    )
    .await;
    assert_eq!(
        seen.path.lock().unwrap().as_deref(),
        Some("/v1/board"),
        "re-prefixing produced /v1/v1/board and a 404 once already"
    );
}

#[tokio::test]
async fn ingress_is_forwarded_under_the_ingress_prefix() {
    let (engine, seen) = mock_engine().await;
    let (app, id) = harness(&engine).await;

    get(
        &app,
        &format!("/host/v1/projects/{id}/ingress/hook/abc"),
        &[],
    )
    .await;
    assert_eq!(
        seen.path.lock().unwrap().as_deref(),
        Some("/ingress/hook/abc")
    );
}

#[tokio::test]
async fn traversal_is_refused_with_an_envelope() {
    let (engine, _) = mock_engine().await;
    let (app, id) = harness(&engine).await;

    let (status, body) = get(
        &app,
        &format!("/host/v1/projects/{id}/engine/v1/../../secret"),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_enveloped(&body, "bad_request");
}

#[tokio::test]
async fn an_unknown_project_is_a_404_envelope() {
    let (engine, _) = mock_engine().await;
    let (app, _id) = harness(&engine).await;

    let (status, body) = get(
        &app,
        &format!("/host/v1/projects/{}/engine/v1/board", Uuid::new_v4()),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_enveloped(&body, "not_found");
}

#[tokio::test]
async fn an_unreachable_engine_is_a_502_envelope_not_a_bare_string() {
    // Port 1 refuses immediately, so this does not wait on a timeout.
    let (app, id) = harness("http://127.0.0.1:1").await;

    let (status, body) = get(
        &app,
        &format!("/host/v1/projects/{id}/engine/v1/board"),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_enveloped(&body, "engine_unreachable");
}

fn assert_enveloped(body: &str, code: &str) {
    let v: serde_json::Value = serde_json::from_str(body)
        .unwrap_or_else(|_| panic!("body was not JSON, so no client can branch on it: {body}"));
    assert_eq!(v["error"]["code"], code, "body was {body}");
    assert!(
        v["error"]["message"].is_string(),
        "every envelope needs a human-readable message: {body}"
    );
}
