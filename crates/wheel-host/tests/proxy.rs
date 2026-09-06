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
        uid_range_start: 20_000,
        uid_stride: 64,
        run_dir: "/tmp/wheel-run-test".into(),
        rlimit_nproc: 4096,
        rlimit_address_space_bytes: None,
        rlimit_fsize_bytes: 8 * 1024 * 1024 * 1024,
        rlimit_nofile: 16384,
        rlimit_cpu_secs: None,
        reap_grace_secs: 1,
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
        auth_limiter: Arc::new(wheel_host::auth_limit::AuthLimiter::new(30)),
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

// --- the websocket bridge -----------------------------------------------------------------------

/// An engine that accepts a socket and echoes frames back unchanged.
async fn mock_engine_ws() -> String {
    let app = Router::new().route(
        "/v1/events",
        axum::routing::get(|ws: axum::extract::ws::WebSocketUpgrade| async move {
            ws.on_upgrade(|mut socket| async move {
                while let Some(Ok(msg)) = socket.recv().await {
                    if socket.send(msg).await.is_err() {
                        break;
                    }
                }
            })
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

/// Serve the host router on a real port; a websocket upgrade cannot travel through `oneshot`.
async fn serve_host(engine_base: &str) -> (String, Uuid) {
    let (app, id) = harness(engine_base).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("ws://{addr}"), id)
}

fn ws_request(url: &str, bearer: &str) -> tokio_tungstenite::tungstenite::http::Request<()> {
    use tokio_tungstenite::tungstenite::handshake::client::generate_key;
    let host = url.split("://").nth(1).unwrap().split('/').next().unwrap();
    tokio_tungstenite::tungstenite::http::Request::builder()
        .uri(url)
        .header("Authorization", format!("Bearer {bearer}"))
        .header("Host", host)
        .header("Connection", "Upgrade")
        .header("Upgrade", "websocket")
        .header("Sec-WebSocket-Version", "13")
        .header("Sec-WebSocket-Key", generate_key())
        .body(())
        .unwrap()
}

#[tokio::test]
async fn websocket_frames_cross_the_host_bridge_verbatim() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;

    let engine = mock_engine_ws().await;
    let (base, id) = serve_host(&engine).await;

    let url = format!("{base}/host/v1/projects/{id}/engine/v1/events");
    let (mut socket, _) = tokio_tungstenite::connect_async(ws_request(&url, SECRET))
        .await
        .expect("the host should bridge a websocket to the engine");

    // The `message` event is correlated by id downstream, so anything that re-encodes JSON on this
    // hop is a defect even when every field survives.
    let payload = r#"{"type":"message","id":"018f-7c1e","body":"quote \" slash \\ ☃ 🎡"}"#;
    socket.send(Message::Text(payload.into())).await.unwrap();
    let echoed = socket.next().await.expect("a reply").expect("no error");
    assert_eq!(echoed.into_text().unwrap().as_str(), payload);
}

#[tokio::test]
async fn the_websocket_bridge_still_requires_the_bearer() {
    let engine = mock_engine_ws().await;
    let (base, id) = serve_host(&engine).await;

    let url = format!("{base}/host/v1/projects/{id}/engine/v1/events");
    assert!(
        tokio_tungstenite::connect_async(ws_request(&url, "wrong-secret"))
            .await
            .is_err(),
        "an upgrade must not be a way around the bearer gate"
    );
}

#[tokio::test]
async fn a_websocket_to_an_unknown_project_is_refused() {
    let engine = mock_engine_ws().await;
    let (base, _id) = serve_host(&engine).await;

    let url = format!(
        "{base}/host/v1/projects/{}/engine/v1/events",
        Uuid::new_v4()
    );
    assert!(tokio_tungstenite::connect_async(ws_request(&url, SECRET))
        .await
        .is_err());
}

/// Every frame type has to survive the bridge, not just text.
///
/// Binary matters because the engine may stream non-JSON payloads. Ping/pong matter more than they
/// look: the events socket is long-lived, and a bridge that swallows keepalives lets an idle
/// connection be reaped by an intermediary — which presents as "the board stopped updating" long
/// after the cause, and is close to undebuggable from the UI side.
#[tokio::test]
async fn binary_frames_cross_the_bridge_unchanged() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;

    let engine = mock_engine_ws().await;
    let (base, id) = serve_host(&engine).await;
    let url = format!("{base}/host/v1/projects/{id}/engine/v1/events");
    let (mut socket, _) = tokio_tungstenite::connect_async(ws_request(&url, SECRET))
        .await
        .expect("bridge should open");

    // Deliberately includes bytes that are not valid UTF-8, so anything treating this as text
    // would corrupt or reject it.
    let payload: Vec<u8> = vec![0x00, 0xff, 0xfe, 0x01, 0x7f, 0x80];
    socket
        .send(Message::Binary(payload.clone().into()))
        .await
        .unwrap();

    let echoed = socket.next().await.expect("a reply").expect("no error");
    match echoed {
        Message::Binary(b) => assert_eq!(b.to_vec(), payload, "binary payload was altered"),
        other => panic!("expected a binary frame back, got {other:?}"),
    }
}

#[tokio::test]
async fn a_close_from_the_client_ends_the_bridge() {
    // Half-open connections are a slow leak: the host holds a socket to the engine for a client
    // that has gone. Closing one side has to tear down the other.
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;

    let engine = mock_engine_ws().await;
    let (base, id) = serve_host(&engine).await;
    let url = format!("{base}/host/v1/projects/{id}/engine/v1/events");
    let (mut socket, _) = tokio_tungstenite::connect_async(ws_request(&url, SECRET))
        .await
        .expect("bridge should open");

    socket.send(Message::Close(None)).await.unwrap();

    // The stream ends rather than hanging. A timeout here would mean the bridge kept the
    // connection alive after the client asked to close it.
    let ended = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while let Some(msg) = socket.next().await {
            if matches!(msg, Ok(Message::Close(_)) | Err(_)) {
                return true;
            }
        }
        true
    })
    .await;
    assert!(
        ended.is_ok(),
        "the bridge did not end after the client closed"
    );
}

#[tokio::test]
async fn a_websocket_without_the_bearer_is_refused() {
    let engine = mock_engine_ws().await;
    let (base, id) = serve_host(&engine).await;
    let url = format!("{base}/host/v1/projects/{id}/engine/v1/events");

    assert!(
        tokio_tungstenite::connect_async(ws_request(&url, "wrong-secret"))
            .await
            .is_err(),
        "the bearer gate does not apply to the websocket route"
    );
}

/// A dead engine must fail the upgrade cleanly.
///
/// The host connects upstream *before* accepting the client's upgrade precisely so this is a 502
/// the caller can read, rather than a websocket that opens and then vanishes — which the browser
/// reports as an ordinary disconnect and tells the user nothing.
#[tokio::test]
async fn a_websocket_to_an_unreachable_engine_is_refused_before_the_upgrade() {
    // Port 1 on loopback: nothing listens, and the connection is refused rather than hanging.
    let (base, id) = serve_host("http://127.0.0.1:1").await;
    let url = format!("{base}/host/v1/projects/{id}/engine/v1/events");

    let err = tokio_tungstenite::connect_async(ws_request(&url, SECRET))
        .await
        .expect_err("an unreachable engine must not produce an open websocket");

    // tungstenite reports the failed upgrade as an HTTP response; the status is the contract.
    if let tokio_tungstenite::tungstenite::Error::Http(resp) = err {
        assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
    } else {
        panic!("expected an HTTP error carrying the status, got {err:?}");
    }
}

/// The websocket route for a project that does not exist must not reveal that it does not exist by
/// behaving differently from one that does: both are a 404 before any upstream work happens.
#[tokio::test]
async fn a_websocket_for_an_unknown_project_is_a_404() {
    let engine = mock_engine_ws().await;
    let (base, _) = serve_host(&engine).await;
    let url = format!(
        "{base}/host/v1/projects/{}/engine/v1/events",
        Uuid::new_v4()
    );

    let err = tokio_tungstenite::connect_async(ws_request(&url, SECRET))
        .await
        .expect_err("an unknown project must not upgrade");
    if let tokio_tungstenite::tungstenite::Error::Http(resp) = err {
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    } else {
        panic!("expected an HTTP error carrying the status, got {err:?}");
    }
}

/// A plain GET on the events path is not an upgrade request. It must be answered, not treated as a
/// websocket: an engine that streams events over a normal response would otherwise be unreachable.
#[tokio::test]
async fn a_non_upgrade_request_to_the_events_path_is_proxied_as_http() {
    let engine = mock_engine().await;
    let (app, id) = harness(&engine.0).await;
    let (status, _) = get(
        &app,
        &format!("/host/v1/projects/{id}/engine/v1/events"),
        &[],
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a non-upgrade GET must take the ordinary http path"
    );
}
