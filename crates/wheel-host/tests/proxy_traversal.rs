// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

//! Path traversal through the host's proxy.
//!
//! Each project's engine here sits beneath its own path on one server. In that layout a parent
//! step in the forwarded path lands in a neighbour's engine, so it is the layout that shows whether
//! the host keeps a request inside the project it was addressed to. Two projects are seeded, and
//! every spelling of `..` a parser might reconstruct is sent at project A over HTTP, over a unix
//! socket and as a WebSocket upgrade. None may reach B, climb from the ingress mount into the
//! control plane, or reach an engine at all.

use async_trait::async_trait;
use axum::body::Body;
use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{FromRequestParts, State};
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Router;
use futures_util::{SinkExt, StreamExt};
use std::sync::{Arc, Mutex};
use tokio_tungstenite::tungstenite::Message;
use tower::ServiceExt;
use uuid::Uuid;
use wheel_host::config::{Backend, Config};
use wheel_host::sandbox::{Sandbox, Secrets, Status};
use wheel_host::{build_router, store::Store, HostState};

const HOST_SECRET: &str = "host-secret-at-least-16-chars";
const SECRET_A: &str = "engine-secret-for-project-a";
const SECRET_B: &str = "engine-secret-for-project-b";

/// `depth` parent steps and then `tail`, spelled every way a parser after axum's single decode
/// might turn back into `..` and `/`.
fn spellings(depth: usize, tail: &[&str]) -> Vec<(&'static str, String)> {
    let spell = |parent: &str, sep: &str| {
        let mut parts = vec![parent; depth];
        parts.extend_from_slice(tail);
        parts.join(sep)
    };
    vec![
        ("literal ..", spell("..", "/")),
        ("encoded %2e%2e", spell("%2e%2e", "/")),
        ("double-encoded %252e%252e", spell("%252e%252e", "/")),
        ("half-encoded .%2e", spell(".%2e", "/")),
        ("mixed-case %2E%2e", spell("%2E%2e", "/")),
        ("encoded slash %2f", spell("..", "%2f")),
        ("double-encoded slash %252f", spell("..", "%252f")),
        (
            "double-encoded dots and slash",
            spell("%252e%252e", "%252f"),
        ),
        ("encoded backslash %5c", spell("..", "%5c")),
        ("double-encoded backslash %255c", spell("..", "%255c")),
        ("tab before ..", spell("%09..", "/")),
    ]
}

/// Paths that climb nowhere but that a parser could still read differently from the check.
const AMBIGUOUS: &[(&str, &str)] = &[
    ("trailing .", "v1/board/."),
    ("trailing %2e", "v1/board/%2e"),
    ("NUL", "v1/bo%00ard"),
    ("empty middle segment", "v1//board"),
    ("literal percent", "v1/100%25"),
];

#[derive(Debug, Clone)]
struct Hit {
    target: String,
    bearer: String,
}

/// Every request any engine received, as its request target and bearer.
#[derive(Clone, Default)]
struct EngineLog(Arc<Mutex<Vec<Hit>>>);

impl EngineLog {
    fn take(&self) -> Vec<Hit> {
        std::mem::take(&mut *self.0.lock().unwrap())
    }
}

/// Records the hit, then echoes a WebSocket if one was asked for, or answers JSON.
async fn record(State(log): State<EngineLog>, req: Request<Body>) -> Response {
    let target = req
        .uri()
        .path_and_query()
        .map(|p| p.as_str().to_string())
        .unwrap_or_default();
    let bearer = req
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    log.0.lock().unwrap().push(Hit { target, bearer });

    let (mut parts, _) = req.into_parts();
    match WebSocketUpgrade::from_request_parts(&mut parts, &()).await {
        Ok(ws) => ws.on_upgrade(|mut socket| async move {
            while let Some(Ok(msg)) = socket.recv().await {
                if socket.send(msg).await.is_err() {
                    break;
                }
            }
        }),
        Err(_) => axum::Json(serde_json::json!({"nodes": []})).into_response(),
    }
}

fn engine_app(log: EngineLog) -> Router {
    Router::new().fallback(record).with_state(log)
}

type EngineBase = Box<dyn Fn(&Uuid) -> String + Send + Sync>;

struct Fake(EngineBase);

#[async_trait]
impl Sandbox for Fake {
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
    fn engine_base(&self, id: &Uuid) -> String {
        (self.0)(id)
    }
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
        engine_base_url: "http://127.0.0.1:1".into(),
        oauth_allowed_projects: Vec::new(),
    }
}

struct Harness {
    app: Router,
    log: EngineLog,
    a: Uuid,
    b: Uuid,
}

/// Short on purpose: a unix socket path has to fit in `sun_path` (~104 bytes) and the macOS temp
/// directory alone nearly fills it.
fn scratch() -> std::path::PathBuf {
    let dir = std::path::PathBuf::from(format!(
        "/tmp/wpt{}",
        &Uuid::new_v4().simple().to_string()[..8]
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

async fn harness(base: EngineBase, log: EngineLog, dir: &std::path::Path) -> Harness {
    let store = Arc::new(Store::open(dir.join("host.db").to_str().unwrap()).unwrap());
    let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
    store.upsert(&a, SECRET_A, "vault-a").await.unwrap();
    store.upsert(&b, SECRET_B, "vault-b").await.unwrap();
    let state = HostState {
        cfg: cfg(),
        sandbox: Arc::new(Fake(base)),
        store,
        http: reqwest::Client::new(),
        auth_limiter: Arc::new(wheel_host::auth_limit::AuthLimiter::new(1_000)),
        ready: wheel_host::Readiness::serving_from_start(),
    };
    Harness {
        app: build_router(state),
        log,
        a,
        b,
    }
}

/// Both projects' engines on one TCP server, each beneath `/tenant/<id>`.
async fn over_tcp() -> Harness {
    let log = EngineLog::default();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = engine_app(log.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let base = format!("http://{addr}");
    harness(
        Box::new(move |id| format!("{base}/tenant/{id}")),
        log,
        &scratch(),
    )
    .await
}

/// An engine on a unix socket, as the process backend runs them.
async fn over_socket() -> Harness {
    let dir = scratch();
    let socket = dir.join("e.sock");
    let log = EngineLog::default();
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let app = engine_app(log.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let base = format!("unix://{}", socket.display());
    harness(Box::new(move |_| base.clone()), log, &dir).await
}

async fn get(app: &Router, uri: &str) -> (StatusCode, String) {
    let req = Request::builder()
        .method("GET")
        .uri(uri)
        .header("authorization", format!("Bearer {HOST_SECRET}"))
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let body = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&body).to_string())
}

/// Send each suffix beneath `prefix`. Returns a line for every one that was not refused or that
/// reached an engine, and every engine hit.
async fn attempt(
    h: &Harness,
    prefix: &str,
    attacks: &[(&'static str, String)],
) -> (Vec<String>, Vec<Hit>) {
    let mut escaped = Vec::new();
    let mut hits = Vec::new();
    for (label, suffix) in attacks {
        let (status, body) = get(&h.app, &format!("{prefix}/{suffix}")).await;
        let reached = h.log.take();
        if status != StatusCode::BAD_REQUEST || !reached.is_empty() {
            escaped.push(format!("{label}: {status} {body}, engine saw {reached:?}"));
        }
        hits.extend(reached);
    }
    (escaped, hits)
}

fn assert_b_untouched(hits: &[Hit], b: &Uuid, escaped: &[String]) {
    let b_prefix = format!("/tenant/{b}/");
    let crossed: Vec<&Hit> = hits
        .iter()
        .filter(|h| h.target.starts_with(&b_prefix) || h.bearer.contains(SECRET_B))
        .collect();
    assert!(
        crossed.is_empty(),
        "a request addressed to project A reached project B's engine: {crossed:?}\nescapes:\n{}",
        escaped.join("\n")
    );
}

#[tokio::test]
async fn no_spelling_of_a_parent_step_reaches_another_projects_engine() {
    let h = over_tcp().await;
    let b = h.b.to_string();
    let (escaped, hits) = attempt(
        &h,
        &format!("/host/v1/projects/{}/engine", h.a),
        &spellings(1, &[&b, "v1", "board"]),
    )
    .await;
    assert_b_untouched(&hits, &h.b, &escaped);
    assert!(
        escaped.is_empty(),
        "not refused before the engine:\n{}",
        escaped.join("\n")
    );
}

#[tokio::test]
async fn the_ingress_mount_never_reaches_another_projects_engine() {
    let h = over_tcp().await;
    let b = h.b.to_string();
    let (escaped, hits) = attempt(
        &h,
        &format!("/host/v1/projects/{}/ingress", h.a),
        &spellings(2, &[&b, "v1", "board"]),
    )
    .await;
    assert_b_untouched(&hits, &h.b, &escaped);
    assert!(
        escaped.is_empty(),
        "not refused before the engine:\n{}",
        escaped.join("\n")
    );
}

/// One step up from the ingress mount is the same engine's control plane, served with the engine
/// secret. The public route must not be a way into it.
#[tokio::test]
async fn the_ingress_mount_cannot_climb_into_the_control_plane() {
    let h = over_tcp().await;
    let (escaped, hits) = attempt(
        &h,
        &format!("/host/v1/projects/{}/ingress", h.a),
        &spellings(1, &["v1", "board"]),
    )
    .await;
    let mount = format!("/tenant/{}/ingress/", h.a);
    let outside: Vec<&Hit> = hits
        .iter()
        .filter(|h| !h.target.starts_with(&mount))
        .collect();
    assert!(
        outside.is_empty(),
        "an ingress request left the ingress mount: {outside:?}"
    );
    assert!(
        escaped.is_empty(),
        "not refused before the engine:\n{}",
        escaped.join("\n")
    );
}

#[tokio::test]
async fn ambiguous_segments_are_refused_with_an_envelope_that_does_not_echo_the_path() {
    let h = over_tcp().await;
    for mount in ["engine", "ingress"] {
        for (label, suffix) in AMBIGUOUS {
            let (status, body) = get(
                &h.app,
                &format!("/host/v1/projects/{}/{mount}/{suffix}", h.a),
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{mount} {label}: {body}");
            let v: serde_json::Value = serde_json::from_str(&body).expect("an envelope");
            assert_eq!(v["error"]["code"], "bad_request", "{mount} {label}");
            assert!(!body.contains(suffix), "{mount} {label} echoed: {body}");
            assert!(
                h.log.take().is_empty(),
                "{mount} {label} reached the engine"
            );
        }
    }
}

#[tokio::test]
async fn ordinary_paths_and_queries_reach_only_the_addressed_engine() {
    let h = over_tcp().await;
    let (a, b) = (h.a, h.b);
    for (uri, target, bearer) in [
        (
            format!("/host/v1/projects/{a}/engine/v1/board"),
            format!("/tenant/{a}/v1/board"),
            SECRET_A,
        ),
        (
            format!("/host/v1/projects/{a}/engine/v1/nodes?dry_run=1&tag=a%2Fb"),
            format!("/tenant/{a}/v1/nodes?dry_run=1&tag=a%2Fb"),
            SECRET_A,
        ),
        (
            format!("/host/v1/projects/{a}/ingress/hook/abc?x=1&y=%2e%2e"),
            format!("/tenant/{a}/ingress/hook/abc?x=1&y=%2e%2e"),
            SECRET_A,
        ),
        (
            format!("/host/v1/projects/{a}/ingress/hook/a%20b/"),
            format!("/tenant/{a}/ingress/hook/a%20b/"),
            SECRET_A,
        ),
        (
            format!("/host/v1/projects/{b}/engine/v1/board"),
            format!("/tenant/{b}/v1/board"),
            SECRET_B,
        ),
    ] {
        let (status, body) = get(&h.app, &uri).await;
        assert_eq!(status, StatusCode::OK, "{uri}: {body}");
        let hits = h.log.take();
        assert_eq!(hits.len(), 1, "{uri}: {hits:?}");
        assert_eq!(hits[0].target, target, "{uri}");
        assert_eq!(hits[0].bearer, format!("Bearer {bearer}"), "{uri}");
    }
}

#[tokio::test]
async fn the_socket_transport_refuses_the_same_paths() {
    let h = over_socket().await;
    let mut attacks = spellings(1, &["v1", "board"]);
    attacks.extend(AMBIGUOUS.iter().map(|(l, s)| (*l, s.to_string())));
    for mount in ["engine", "ingress"] {
        let (escaped, _) =
            attempt(&h, &format!("/host/v1/projects/{}/{mount}", h.a), &attacks).await;
        assert!(
            escaped.is_empty(),
            "{mount} over the socket, not refused:\n{}",
            escaped.join("\n")
        );
    }
}

#[tokio::test]
async fn the_socket_transport_still_forwards_ordinary_paths_and_queries() {
    let h = over_socket().await;
    let a = h.a;
    for (uri, target) in [
        (
            format!("/host/v1/projects/{a}/engine/v1/nodes?dry_run=1"),
            "/v1/nodes?dry_run=1",
        ),
        (
            format!("/host/v1/projects/{a}/ingress/hook?x=1"),
            "/ingress/hook?x=1",
        ),
        (
            format!("/host/v1/projects/{a}/ingress/a%20b"),
            "/ingress/a%20b",
        ),
    ] {
        let (status, body) = get(&h.app, &uri).await;
        assert_eq!(status, StatusCode::OK, "{uri}: {body}");
        let hits = h.log.take();
        assert_eq!(hits.len(), 1, "{uri}: {hits:?}");
        assert_eq!(hits[0].target, target, "{uri}");
        assert_eq!(hits[0].bearer, format!("Bearer {SECRET_A}"), "{uri}");
    }
}

// --- websockets ---------------------------------------------------------------------------------

async fn serve(app: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("ws://{addr}")
}

fn ws_request(url: &str) -> tokio_tungstenite::tungstenite::http::Request<()> {
    use tokio_tungstenite::tungstenite::handshake::client::generate_key;
    let host = url.split("://").nth(1).unwrap().split('/').next().unwrap();
    tokio_tungstenite::tungstenite::http::Request::builder()
        .uri(url)
        .header("Authorization", format!("Bearer {HOST_SECRET}"))
        .header("Host", host)
        .header("Connection", "Upgrade")
        .header("Upgrade", "websocket")
        .header("Sec-WebSocket-Version", "13")
        .header("Sec-WebSocket-Key", generate_key())
        .body(())
        .unwrap()
}

#[tokio::test]
async fn a_websocket_on_a_traversing_path_is_refused_before_any_engine_is_dialled() {
    let h = over_tcp().await;
    let base = serve(h.app.clone()).await;
    let url = format!(
        "{base}/host/v1/projects/{}/engine/%252e%252e/{}/v1/events",
        h.a, h.b
    );
    let err = tokio_tungstenite::connect_async(ws_request(&url))
        .await
        .expect_err("a traversing upgrade must not open");
    match err {
        tokio_tungstenite::tungstenite::Error::Http(resp) => {
            assert_eq!(resp.status(), StatusCode::BAD_REQUEST)
        }
        other => panic!("expected an HTTP refusal, got {other:?}"),
    }
    let hits = h.log.take();
    assert!(hits.is_empty(), "an engine was dialled: {hits:?}");
}

#[tokio::test]
async fn websockets_still_bridge_over_both_transports() {
    let tcp = over_tcp().await;
    let tcp_target = format!("/tenant/{}/v1/events?since=3", tcp.a);
    for (h, target) in [
        (tcp, tcp_target),
        (over_socket().await, "/v1/events?since=3".to_string()),
    ] {
        let base = serve(h.app.clone()).await;
        let url = format!("{base}/host/v1/projects/{}/engine/v1/events?since=3", h.a);
        let (mut socket, _) = tokio_tungstenite::connect_async(ws_request(&url))
            .await
            .expect("the bridge should open");
        socket.send(Message::Text("ping".into())).await.unwrap();
        let echoed = socket.next().await.expect("a reply").expect("no error");
        assert_eq!(echoed.into_text().unwrap().as_str(), "ping");

        let hits = h.log.take();
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert_eq!(hits[0].target, target);
        assert_eq!(hits[0].bearer, format!("Bearer {SECRET_A}"));
    }
}

/// The `%` refusal in `wheel_core::proxy_path` is sound only because axum decodes a wildcard
/// exactly once: a second decode would hand the proxy something the check never saw. This pins
/// the extractor shape the proxy uses, so an axum upgrade that changes it goes red here.
#[tokio::test]
async fn axum_decodes_the_wildcard_exactly_once() {
    let app =
        Router::new().route(
            "/{id}/{*rest}",
            axum::routing::any(
                |axum::extract::Path((_, rest)): axum::extract::Path<(Uuid, String)>| async move {
                    rest
                },
            ),
        );
    let id = Uuid::new_v4();
    for (raw, decoded) in [
        ("%2525", "%25"),
        ("%252e%252e/x", "%2e%2e/x"),
        ("%2e%2e/x", "../x"),
        ("a%2fb", "a/b"),
        ("a%5cb", "a\\b"),
        ("a%20b", "a b"),
    ] {
        let (status, body) = get(&app, &format!("/{id}/{raw}")).await;
        assert_eq!(status, StatusCode::OK, "{raw}");
        assert_eq!(body, decoded, "{raw}");
    }
}
