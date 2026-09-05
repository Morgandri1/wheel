//! `HostClient` against a mock host.
//!
//! This is the seam where the API stops being in control: everything past it is another process
//! that can be slow, wrong, or gone. The behaviour that matters is therefore mostly about failure —
//! that the bearer is attached, that a missing sandbox converges rather than erupting, and that
//! retries happen only where a repeat is safe.

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use uuid::Uuid;
use wheel_api::crypto::Secret;
use wheel_api::models::ProjectStatus;
use wheel_api::orchestrator::{host::HostClient, EngineSecrets, Orchestrator};

const SECRET: &str = "host-secret-value";

#[derive(Clone, Default)]
struct MockState {
    hits: Arc<AtomicUsize>,
    bearers: Arc<Mutex<Vec<String>>>,
    /// Status code the mock returns for lifecycle calls.
    lifecycle_status: Arc<Mutex<StatusCode>>,
    /// Body returned by GET /projects/:id.
    status_body: Arc<Mutex<Value>>,
    status_code: Arc<Mutex<StatusCode>>,
}

fn record(state: &MockState, headers: &HeaderMap) {
    state.hits.fetch_add(1, Ordering::SeqCst);
    let b = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    state.bearers.lock().unwrap().push(b);
}

async fn mock_host() -> (String, MockState) {
    let state = MockState {
        lifecycle_status: Arc::new(Mutex::new(StatusCode::OK)),
        status_body: Arc::new(Mutex::new(json!({"status": "running"}))),
        status_code: Arc::new(Mutex::new(StatusCode::OK)),
        ..Default::default()
    };

    let app = Router::new()
        .route(
            "/host/v1/projects/{id}",
            get(
                |State(s): State<MockState>, headers: HeaderMap, Path(_): Path<Uuid>| async move {
                    record(&s, &headers);
                    let code = *s.status_code.lock().unwrap();
                    let body = s.status_body.lock().unwrap().clone();
                    (code, Json(body))
                },
            )
            .put(
                |State(s): State<MockState>, headers: HeaderMap, Path(_): Path<Uuid>, _b: Json<Value>| async move {
                    record(&s, &headers);
                    (*s.lifecycle_status.lock().unwrap(), Json(json!({"ok": true})))
                },
            )
            .delete(
                |State(s): State<MockState>, headers: HeaderMap, Path(_): Path<Uuid>| async move {
                    record(&s, &headers);
                    (*s.lifecycle_status.lock().unwrap(), Json(json!({"ok": true})))
                },
            ),
        )
        .route(
            "/host/v1/projects/{id}/{action}",
            post(
                |State(s): State<MockState>, headers: HeaderMap, Path(_): Path<(Uuid, String)>| async move {
                    record(&s, &headers);
                    (*s.lifecycle_status.lock().unwrap(), Json(json!({"ok": true})))
                },
            ),
        )
        .with_state(state.clone());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), state)
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
        engine_secret: Secret::new("engine-secret"),
        vault_key: Secret::new("vault-key"),
    }
}

#[tokio::test]
async fn every_call_carries_the_host_bearer() {
    // The host accepts nothing without it, and it must never be omitted on any verb.
    let (base, state) = mock_host().await;
    let c = client(&base);
    let id = Uuid::new_v4();

    c.provision(&id, &secrets()).await.unwrap();
    c.start(&id).await.unwrap();
    c.stop(&id).await.unwrap();
    c.restart(&id).await.unwrap();
    c.destroy(&id).await.unwrap();
    c.status(&id).await.unwrap();

    let bearers = state.bearers.lock().unwrap();
    assert!(!bearers.is_empty());
    for b in bearers.iter() {
        assert_eq!(
            b,
            &format!("Bearer {SECRET}"),
            "a call went out without the host bearer"
        );
    }
}

#[tokio::test]
async fn status_maps_every_host_state() {
    let (base, state) = mock_host().await;
    let c = client(&base);
    let id = Uuid::new_v4();

    for (given, expected) in [
        ("running", ProjectStatus::Running),
        ("starting", ProjectStatus::Starting),
        ("stopped", ProjectStatus::Stopped),
        ("error", ProjectStatus::Error),
        // Anything unrecognised is an error rather than an optimistic guess.
        ("something-new", ProjectStatus::Error),
    ] {
        *state.status_body.lock().unwrap() = json!({ "status": given });
        assert_eq!(c.status(&id).await.unwrap(), expected, "status {given}");
    }
}

#[tokio::test]
async fn a_sandbox_the_host_has_never_heard_of_is_stopped_not_an_error() {
    // A project row can exist before its sandbox does. Treating that as a hard error would make
    // the board unreadable for a project that simply has not been started yet.
    let (base, state) = mock_host().await;
    *state.status_code.lock().unwrap() = StatusCode::NOT_FOUND;
    assert_eq!(
        client(&base).status(&Uuid::new_v4()).await.unwrap(),
        ProjectStatus::Stopped
    );
}

#[tokio::test]
async fn destroying_an_absent_sandbox_succeeds() {
    // Delete has to converge: if the sandbox is already gone, that is the desired end state, and
    // failing here would strand the project row forever.
    let (base, state) = mock_host().await;
    *state.lifecycle_status.lock().unwrap() = StatusCode::NOT_FOUND;
    client(&base).destroy(&Uuid::new_v4()).await.unwrap();
}

#[tokio::test]
async fn host_errors_surface_as_errors() {
    let (base, state) = mock_host().await;
    *state.lifecycle_status.lock().unwrap() = StatusCode::INTERNAL_SERVER_ERROR;
    let c = client(&base);
    let id = Uuid::new_v4();
    assert!(c.start(&id).await.is_err());
    assert!(c.stop(&id).await.is_err());
    assert!(c.restart(&id).await.is_err());
}

#[tokio::test]
async fn idempotent_calls_retry_and_single_shot_calls_do_not() {
    // Retrying a repeat-safe call is resilience; retrying anything else is a way to perform an
    // operation twice. provision (PUT) and destroy (DELETE) are idempotent by contract; stop is
    // sent exactly once.
    let (base, state) = mock_host().await;
    *state.lifecycle_status.lock().unwrap() = StatusCode::INTERNAL_SERVER_ERROR;
    let c = client(&base);
    let id = Uuid::new_v4();

    state.hits.store(0, Ordering::SeqCst);
    let _ = c.provision(&id, &secrets()).await;
    assert!(
        state.hits.load(Ordering::SeqCst) > 1,
        "provision is idempotent and should have retried"
    );

    state.hits.store(0, Ordering::SeqCst);
    let _ = c.stop(&id).await;
    assert_eq!(
        state.hits.load(Ordering::SeqCst),
        1,
        "stop must be attempted exactly once"
    );
}

#[tokio::test]
async fn an_unreachable_host_is_an_error_not_a_hang() {
    // Port 1 on loopback refuses immediately, standing in for a host that is down.
    let c = client("http://127.0.0.1:1");
    let id = Uuid::new_v4();
    assert!(
        c.status(&id).await.is_err(),
        "an unreachable host must not read as a status"
    );
    assert!(c.start(&id).await.is_err());
    assert!(c.provision(&id, &secrets()).await.is_err());
}

#[tokio::test]
async fn malformed_status_body_is_an_error() {
    let (base, state) = mock_host().await;
    *state.status_body.lock().unwrap() = json!({ "unexpected": "shape" });
    assert!(client(&base).status(&Uuid::new_v4()).await.is_err());
}
