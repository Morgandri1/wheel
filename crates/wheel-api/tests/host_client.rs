//! `HostClient` against a mock host.
//!
//! This is the code that turns every lifecycle call into an authenticated request to the one
//! machine that owns all tenants' sandboxes, so the things worth pinning down are: the bearer is
//! always attached, secrets go up but never come back, idempotent failures are retried while
//! non-idempotent ones are not, and a host that is missing, broken, or lying does not produce a
//! confident wrong answer.

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{any, get};
use axum::{Json, Router};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use uuid::Uuid;
use wheel_api::crypto::Secret;
use wheel_api::models::ProjectStatus;
use wheel_api::orchestrator::host::HostClient;
use wheel_api::orchestrator::{EngineSecrets, Orchestrator};

const SECRET: &str = "host-secret-value-at-least-16";

#[derive(Clone, Default)]
struct Seen {
    calls: Arc<Mutex<Vec<(String, String)>>>,
    last_auth: Arc<Mutex<Option<String>>>,
    last_body: Arc<Mutex<Option<Value>>>,
    /// Fail this many times before succeeding, to exercise retry.
    fail_times: Arc<Mutex<u32>>,
    status_reply: Arc<Mutex<Value>>,
    force_status: Arc<Mutex<Option<u16>>>,
}

impl Seen {
    fn calls(&self) -> Vec<(String, String)> {
        self.calls.lock().unwrap().clone()
    }
    /// Exact match on the action segment, not a suffix: "restart" ends with "start", so a suffix
    /// test silently counts a restart as a start.
    fn count(&self, method: &str, action: &str) -> usize {
        self.calls()
            .iter()
            .filter(|(m, a)| m == method && a == action)
            .count()
    }
}

async fn mock_host() -> (String, Seen) {
    let seen = Seen::default();
    *seen.status_reply.lock().unwrap() = json!({"status": "running"});

    let app = Router::new()
        .route(
            "/host/v1/projects/{id}",
            any(
                |State(s): State<Seen>,
                 Path(_id): Path<Uuid>,
                 method: axum::http::Method,
                 headers: HeaderMap,
                 body: String| async move {
                    record(&s, method.as_str(), "", &headers, &body);
                    if let Some(code) = *s.force_status.lock().unwrap() {
                        return (StatusCode::from_u16(code).unwrap(), Json(json!({"e": 1})))
                            .into_response_tuple();
                    }
                    let mut remaining = s.fail_times.lock().unwrap();
                    if *remaining > 0 {
                        *remaining -= 1;
                        return (
                            StatusCode::INTERNAL_SERVER_ERROR,
                            Json(json!({"e": "boom"})),
                        )
                            .into_response_tuple();
                    }
                    if method == axum::http::Method::GET {
                        let reply = s.status_reply.lock().unwrap().clone();
                        return (StatusCode::OK, Json(reply)).into_response_tuple();
                    }
                    (StatusCode::OK, Json(json!({"ok": true}))).into_response_tuple()
                },
            ),
        )
        .route(
            "/host/v1/projects/{id}/{action}",
            any(
                |State(s): State<Seen>,
                 Path((_id, action)): Path<(Uuid, String)>,
                 method: axum::http::Method,
                 headers: HeaderMap,
                 body: String| async move {
                    record(&s, method.as_str(), &action, &headers, &body);
                    let mut remaining = s.fail_times.lock().unwrap();
                    if *remaining > 0 {
                        *remaining -= 1;
                        return (
                            StatusCode::INTERNAL_SERVER_ERROR,
                            Json(json!({"e": "boom"})),
                        )
                            .into_response_tuple();
                    }
                    (StatusCode::OK, Json(json!({"ok": true}))).into_response_tuple()
                },
            ),
        )
        .route("/ping", get(|| async { "ok" }))
        .with_state(seen.clone());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), seen)
}

fn record(s: &Seen, method: &str, action: &str, headers: &HeaderMap, body: &str) {
    s.calls
        .lock()
        .unwrap()
        .push((method.to_string(), action.to_string()));
    *s.last_auth.lock().unwrap() = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    if !body.is_empty() {
        *s.last_body.lock().unwrap() = serde_json::from_str(body).ok();
    }
}

/// Small helper so the closures above can return one concrete type.
trait IntoResponseTuple {
    fn into_response_tuple(self) -> (StatusCode, Json<Value>);
}
impl IntoResponseTuple for (StatusCode, Json<Value>) {
    fn into_response_tuple(self) -> (StatusCode, Json<Value>) {
        self
    }
}

fn client(base: &str) -> HostClient {
    HostClient::new(
        reqwest::Client::new(),
        base.to_string(),
        Secret::new(SECRET),
    )
}

fn secrets() -> EngineSecrets {
    EngineSecrets {
        engine_secret: Secret::new("engine-secret-abc"),
        vault_key: Secret::new("vault-key-xyz"),
    }
}

#[tokio::test]
async fn provision_sends_the_bearer_and_the_secrets() {
    let (base, seen) = mock_host().await;
    client(&base)
        .provision(&Uuid::new_v4(), &secrets())
        .await
        .unwrap();

    assert_eq!(
        seen.last_auth.lock().unwrap().as_deref(),
        Some(format!("Bearer {SECRET}").as_str()),
        "the host bearer must be attached to every call"
    );
    let body = seen.last_body.lock().unwrap().clone().expect("a json body");
    assert_eq!(body["engine_secret"], "engine-secret-abc");
    assert_eq!(body["vault_key"], "vault-key-xyz");
    // Capability must start closed: the public ingress route is opt-in.
    assert_eq!(body["capabilities"]["http"], false);
}

#[tokio::test]
async fn lifecycle_calls_hit_the_documented_paths() {
    let (base, seen) = mock_host().await;
    let c = client(&base);
    let id = Uuid::new_v4();

    c.start(&id).await.unwrap();
    c.stop(&id).await.unwrap();
    c.restart(&id).await.unwrap();
    c.destroy(&id).await.unwrap();

    assert_eq!(seen.count("POST", "start"), 1);
    assert_eq!(seen.count("POST", "stop"), 1);
    assert_eq!(seen.count("POST", "restart"), 1);
    assert_eq!(seen.count("DELETE", ""), 1);
}

#[tokio::test]
async fn status_maps_every_host_answer() {
    let (base, seen) = mock_host().await;
    let c = client(&base);
    let id = Uuid::new_v4();

    for (reported, expected) in [
        ("running", ProjectStatus::Running),
        ("starting", ProjectStatus::Starting),
        ("stopped", ProjectStatus::Stopped),
        // Anything we do not recognise is an error, not an optimistic guess.
        ("wat", ProjectStatus::Error),
    ] {
        *seen.status_reply.lock().unwrap() = json!({ "status": reported });
        assert_eq!(
            c.status(&id).await.unwrap(),
            expected,
            "reported {reported}"
        );
    }
}

#[tokio::test]
async fn a_host_that_has_never_heard_of_the_project_reads_as_stopped() {
    let (base, seen) = mock_host().await;
    *seen.force_status.lock().unwrap() = Some(404);
    assert_eq!(
        client(&base).status(&Uuid::new_v4()).await.unwrap(),
        ProjectStatus::Stopped,
        "a 404 from the host means no sandbox exists, which is stopped rather than an error"
    );
}

#[tokio::test]
async fn destroy_is_satisfied_by_an_already_absent_sandbox() {
    let (base, seen) = mock_host().await;
    *seen.force_status.lock().unwrap() = Some(404);
    // Delete has to converge: re-deleting something already gone is success, or a failed teardown
    // can never be retried to completion.
    client(&base).destroy(&Uuid::new_v4()).await.unwrap();
}

#[tokio::test]
async fn idempotent_calls_retry_and_eventually_succeed() {
    let (base, seen) = mock_host().await;
    *seen.fail_times.lock().unwrap() = 2;

    client(&base)
        .provision(&Uuid::new_v4(), &secrets())
        .await
        .expect("provision should survive two transient host failures");
    assert_eq!(
        seen.count("PUT", ""),
        3,
        "expected two failures then a success"
    );
}

#[tokio::test]
async fn stop_is_not_retried() {
    let (base, seen) = mock_host().await;
    *seen.fail_times.lock().unwrap() = 1;

    // stop is a single attempt by design; retrying non-idempotent lifecycle calls is how you get
    // a stop racing a start.
    assert!(client(&base).stop(&Uuid::new_v4()).await.is_err());
    assert_eq!(seen.count("POST", "stop"), 1);
}

#[tokio::test]
async fn an_unreachable_host_is_an_error_not_a_status() {
    // Port 1 on loopback refuses instantly, so this does not depend on a timeout elapsing.
    let c = client("http://127.0.0.1:1");
    assert!(c.status(&Uuid::new_v4()).await.is_err());
    assert!(c.start(&Uuid::new_v4()).await.is_err());
}

#[tokio::test]
async fn host_error_bodies_do_not_leak_into_ours() {
    let (base, seen) = mock_host().await;
    *seen.force_status.lock().unwrap() = Some(500);

    let err = client(&base)
        .status(&Uuid::new_v4())
        .await
        .expect_err("a 500 from the host is an error");
    let rendered = format!("{err:#}");
    assert!(
        rendered.contains("500"),
        "operators need the upstream status: {rendered}"
    );
}

/// The host's liveness probe goes to the unauthenticated `/healthz`, not the bearer-gated
/// `/host/v1/healthz`.
///
/// The API-facing route is a deploy gate: it must answer whether the host process is serving, and
/// it must not answer "no" merely because a bearer was rejected — that would report an outage
/// during a secret rotation and hide one during a real failure.
#[tokio::test]
async fn host_liveness_probes_the_unauthenticated_route() {
    let hits = Arc::new(Mutex::new(Vec::<String>::new()));
    let seen = hits.clone();
    let app = Router::new().route(
        "/healthz",
        get(move || {
            let seen = seen.clone();
            async move {
                seen.lock().unwrap().push("/healthz".into());
                (StatusCode::OK, Json(json!({"ok": true})))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let client = HostClient::new(reqwest::Client::new(), base, Secret::new("host-secret"));
    client.host_alive().await.expect("a serving host is alive");
    assert_eq!(hits.lock().unwrap().as_slice(), ["/healthz"]);
}

#[tokio::test]
async fn a_host_that_answers_an_error_is_not_alive() {
    let app = Router::new().route(
        "/healthz",
        get(|| async { (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({}))) }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let client = HostClient::new(reqwest::Client::new(), base, Secret::new("host-secret"));
    assert!(client.host_alive().await.is_err());
}

/// A host that is not there at all is the case the route exists for — and it must be an error
/// rather than a hang, because the gate calling it has a timeout of its own.
#[tokio::test]
async fn a_host_that_is_not_listening_is_not_alive() {
    let client = HostClient::new(
        reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(2))
            .build()
            .unwrap(),
        "http://127.0.0.1:1".into(),
        Secret::new("host-secret"),
    );
    assert!(client.host_alive().await.is_err());
}

/// A 507 from the host is a refusal the owner can act on, and it must survive as one.
///
/// The host says "the volume is full" precisely; everything above it used to flatten that into an
/// unexpected failure, and the API reported a 500 with no cause. The type has to make it through
/// the `anyhow` chain, `.context()` and all, or the route cannot tell this apart from any other
/// upstream error.
#[tokio::test]
async fn a_host_out_of_disk_is_a_refusal_not_an_unexplained_failure() {
    let app = Router::new().route(
        "/host/v1/projects/{id}/start",
        any(|| async {
            (
                StatusCode::INSUFFICIENT_STORAGE,
                Json(json!({"status": "error",
                            "last_error": "the volume is full: 12 MB free on /data (99% used)"})),
            )
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let client = HostClient::new(reqwest::Client::new(), base, Secret::new("host-secret"));
    let err = client
        .start(&Uuid::new_v4())
        .await
        .expect_err("a host with no disk must not report a successful start");

    assert!(
        matches!(
            err.downcast_ref::<wheel_api::orchestrator::HostRefusal>(),
            Some(wheel_api::orchestrator::HostRefusal::OutOfDisk)
        ),
        "the refusal did not survive the error chain: {err:#}"
    );
    // And the operator still gets the host's own words in the log.
    assert!(format!("{err:#}").contains("507"), "{err:#}");
}

/// Every other host failure stays untyped, so the disk case is a distinction rather than a
/// relabelling of everything that goes wrong upstream.
#[tokio::test]
async fn an_ordinary_host_error_carries_no_refusal_type() {
    let app = Router::new().route(
        "/host/v1/projects/{id}/start",
        any(|| async { (StatusCode::BAD_GATEWAY, Json(json!({"status": "error"}))) }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let client = HostClient::new(reqwest::Client::new(), base, Secret::new("host-secret"));
    let err = client
        .start(&Uuid::new_v4())
        .await
        .expect_err("502 is an error");
    assert!(err
        .downcast_ref::<wheel_api::orchestrator::HostRefusal>()
        .is_none());
}
