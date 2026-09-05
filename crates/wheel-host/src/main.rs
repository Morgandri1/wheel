//! `wheel-host` — the sandbox supervisor (ARCHITECTURE §4b).
//!
//! Runs on the single engine machine, one instance only. Owns every project's sandbox and is the
//! only process that holds engine secrets at runtime. It has no public domain: the API reaches it
//! over private networking with a bearer secret, and nothing else may.
//!
//! The security posture is blunt on purpose. Anything that can reach this port can control every
//! tenant's sandbox, so the bearer check is the first thing in the stack and applies to every
//! route including the proxies.

mod config;
mod proxy;
mod sandbox;
mod store;

use anyhow::{Context, Result};
use axum::extract::{Path, Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, put};
use axum::{Json, Router};
use config::{Backend, Config};
use sandbox::{Sandbox, Secrets};
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;
use uuid::Uuid;

#[derive(Clone)]
pub(crate) struct HostState {
    pub cfg: Config,
    pub sandbox: Arc<dyn Sandbox>,
    pub store: Arc<store::Store>,
    pub http: reqwest::Client,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,wheel_host=debug".into()),
        )
        .init();

    let cfg = Config::from_env().context("configuration")?;
    tracing::info!(backend = ?cfg.backend, "wheel-host starting");

    let sandbox: Arc<dyn Sandbox> = match cfg.backend {
        Backend::Docker => Arc::new(sandbox::docker::DockerSandbox::connect(cfg.clone())?),
        Backend::Process => {
            // M3. Failing loudly beats starting with a backend that silently does nothing.
            anyhow::bail!("the process sandbox backend is not implemented yet (M3)")
        }
    };

    let store = Arc::new(store::Store::open(&format!("{}/host.db", cfg.data_dir))?);

    let state = HostState {
        cfg: cfg.clone(),
        sandbox,
        store: store.clone(),
        http: reqwest::Client::new(),
    };

    reconcile_on_boot(&state).await;

    let app = Router::new()
        .route("/host/v1/healthz", get(healthz))
        .route("/host/v1/projects/{id}", put(put_project).get(get_project).delete(delete_project))
        .route("/host/v1/projects/{id}/start", axum::routing::post(start))
        .route("/host/v1/projects/{id}/stop", axum::routing::post(stop))
        .route("/host/v1/projects/{id}/restart", axum::routing::post(restart))
        .route("/host/v1/projects/{id}/engine/{*rest}", any(proxy::engine))
        .route("/host/v1/projects/{id}/ingress/{*rest}", any(proxy::ingress))
        .layer(axum::middleware::from_fn_with_state(state.clone(), require_bearer))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(&cfg.bind_addr).await?;
    tracing::info!(addr = %cfg.bind_addr, "listening");
    axum::serve(listener, app).await?;
    Ok(())
}

/// Restart whatever was running before we went down.
///
/// The host is deliberately a single instance, so nothing else will notice that a project's engine
/// died with us. Without this, a host restart silently stops every tenant's agents.
async fn reconcile_on_boot(state: &HostState) {
    let wanted = match state.store.all_desired_running().await {
        Ok(w) => w,
        Err(e) => {
            tracing::error!(error = ?e, "could not read desired state; skipping reconcile");
            return;
        }
    };
    tracing::info!(count = wanted.len(), "reconciling projects that were running");
    for rec in wanted {
        let secrets = Secrets {
            engine_secret: rec.engine_secret.clone(),
            vault_key: rec.vault_key.clone(),
        };
        if let Err(e) = state.sandbox.start(&rec.id, &secrets).await {
            // One project failing to come back must not stop the others.
            tracing::error!(project = %rec.id, error = ?e, "reconcile: start failed");
        }
    }
}

/// Bearer check, applied to every route.
///
/// Compared in constant time: a byte-by-byte early-exit comparison leaks the secret's prefix to
/// anyone who can measure response timing across many attempts.
async fn require_bearer(State(state): State<HostState>, req: Request, next: Next) -> Response {
    let presented = req
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");

    if !constant_time_eq(presented.as_bytes(), state.cfg.secret.as_bytes()) {
        tracing::warn!("rejected host request with bad or missing bearer");
        return (StatusCode::UNAUTHORIZED, Json(json!({"error": "unauthorized"}))).into_response();
    }
    next.run(req).await
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

async fn healthz(State(state): State<HostState>) -> Json<serde_json::Value> {
    Json(json!({
        "ok": true,
        "sandbox_backend": match state.cfg.backend {
            Backend::Docker => "docker",
            Backend::Process => "process",
        },
    }))
}

#[derive(Deserialize)]
struct PutProject {
    engine_secret: String,
    vault_key: String,
}

/// Create-or-update a project's sandbox record. Idempotent, per §4b.
async fn put_project(
    State(state): State<HostState>,
    Path(id): Path<Uuid>,
    Json(body): Json<PutProject>,
) -> Response {
    if let Err(e) = state
        .store
        .upsert(&id, &body.engine_secret, &body.vault_key)
        .await
    {
        return internal(e, "storing project record");
    }
    let secrets = Secrets {
        engine_secret: body.engine_secret,
        vault_key: body.vault_key,
    };
    match state.sandbox.provision(&id, &secrets).await {
        Ok(()) => (StatusCode::OK, Json(json!({"ok": true}))).into_response(),
        Err(e) => internal(e, "provisioning sandbox"),
    }
}

async fn get_project(State(state): State<HostState>, Path(id): Path<Uuid>) -> Response {
    match state.store.get(&id).await {
        Err(e) => internal(e, "reading project record"),
        Ok(None) => (StatusCode::NOT_FOUND, Json(json!({"error": "not_found"}))).into_response(),
        Ok(Some(_)) => match state.sandbox.status(&id).await {
            Ok(status) => (StatusCode::OK, Json(json!({ "status": status }))).into_response(),
            Err(e) => {
                tracing::warn!(project = %id, error = ?e, "status probe failed");
                (
                    StatusCode::OK,
                    Json(json!({ "status": "error", "last_error": e.to_string() })),
                )
                    .into_response()
            }
        },
    }
}

async fn start(State(state): State<HostState>, Path(id): Path<Uuid>) -> Response {
    let Some(rec) = (match state.store.get(&id).await {
        Ok(r) => r,
        Err(e) => return internal(e, "reading project record"),
    }) else {
        return (StatusCode::NOT_FOUND, Json(json!({"error": "not_found"}))).into_response();
    };

    let secrets = Secrets {
        engine_secret: rec.engine_secret,
        vault_key: rec.vault_key,
    };
    match state.sandbox.start(&id, &secrets).await {
        Ok(()) => {
            // Record intent only after the sandbox is actually up, so a failed start does not make
            // us try to resurrect a broken project on every future boot.
            if let Err(e) = state.store.set_desired_running(&id, true).await {
                tracing::error!(project = %id, error = ?e, "could not persist desired state");
            }
            (StatusCode::OK, Json(json!({"status": "running"}))).into_response()
        }
        Err(e) => {
            tracing::warn!(project = %id, error = ?e, "start failed");
            (
                StatusCode::GATEWAY_TIMEOUT,
                Json(json!({"status": "error", "last_error": e.to_string()})),
            )
                .into_response()
        }
    }
}

async fn stop(State(state): State<HostState>, Path(id): Path<Uuid>) -> Response {
    if let Err(e) = state.store.set_desired_running(&id, false).await {
        return internal(e, "persisting desired state");
    }
    match state.sandbox.stop(&id).await {
        Ok(()) => (StatusCode::OK, Json(json!({"status": "stopped"}))).into_response(),
        Err(e) => internal(e, "stopping sandbox"),
    }
}

async fn restart(State(state): State<HostState>, Path(id): Path<Uuid>) -> Response {
    let Some(rec) = (match state.store.get(&id).await {
        Ok(r) => r,
        Err(e) => return internal(e, "reading project record"),
    }) else {
        return (StatusCode::NOT_FOUND, Json(json!({"error": "not_found"}))).into_response();
    };
    let secrets = Secrets {
        engine_secret: rec.engine_secret,
        vault_key: rec.vault_key,
    };
    match state.sandbox.restart(&id, &secrets).await {
        Ok(()) => (StatusCode::OK, Json(json!({"status": "running"}))).into_response(),
        Err(e) => internal(e, "restarting sandbox"),
    }
}

async fn delete_project(State(state): State<HostState>, Path(id): Path<Uuid>) -> Response {
    // Destroy the runtime first: a sandbox whose record we already deleted is one nothing will
    // ever clean up.
    if let Err(e) = state.sandbox.destroy(&id).await {
        return internal(e, "destroying sandbox");
    }
    match state.store.delete(&id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => internal(e, "deleting project record"),
    }
}

fn internal(e: anyhow::Error, what: &str) -> Response {
    tracing::error!(error = ?e, "{what} failed");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({"error": "internal"})),
    )
        .into_response()
}
