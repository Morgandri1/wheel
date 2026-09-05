//! `wheel-host` — the sandbox supervisor (ARCHITECTURE §4b).
//!
//! Runs on the single engine machine. Owns every project's sandbox and is the only process that
//! holds engine secrets at runtime. It has no public domain: the API reaches it over private
//! networking with a bearer token, and that token is the only thing between anything that can
//! reach this port and full control of every tenant's sandbox.

mod config;
mod sandbox;
mod store;

use anyhow::Result;
use axum::extract::{Path, Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, put};
use axum::{Json, Router};
use config::{Backend, Config};
use sandbox::{Sandbox, Secrets, Status};
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;
use store::Store;
use uuid::Uuid;

#[derive(Clone)]
struct HostState(Arc<Inner>);

struct Inner {
    cfg: Config,
    store: Store,
    sandbox: Box<dyn Sandbox>,
    http: reqwest::Client,
}

impl std::ops::Deref for HostState {
    type Target = Inner;
    fn deref(&self) -> &Inner {
        &self.0
    }
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

    let cfg = Config::from_env()?;
    tracing::info!(backend = ?cfg.backend, "wheel-host starting");

    let sandbox: Box<dyn Sandbox> = match cfg.backend {
        Backend::Docker => Box::new(sandbox::docker::DockerSandbox::connect(cfg.clone())?),
        Backend::Process => {
            // M3. Failing loudly beats silently falling back to a weaker isolation model.
            anyhow::bail!("SANDBOX_BACKEND=process is not implemented yet (M3)")
        }
    };

    let store = Store::open(&format!("{}/host.db", cfg.data_dir))?;
    let state = HostState(Arc::new(Inner {
        cfg: cfg.clone(),
        store,
        sandbox,
        http: reqwest::Client::new(),
    }));

    reconcile_on_boot(&state).await;

    let app = Router::new()
        .route("/host/v1/healthz", get(healthz))
        .route("/host/v1/projects/{id}", put(provision).get(status).delete(destroy))
        .route("/host/v1/projects/{id}/start", axum::routing::post(start))
        .route("/host/v1/projects/{id}/stop", axum::routing::post(stop))
        .route("/host/v1/projects/{id}/restart", axum::routing::post(restart))
        .route("/host/v1/projects/{id}/engine/{*rest}", any(proxy_engine))
        .route("/host/v1/projects/{id}/ingress/{*rest}", any(proxy_ingress))
        .layer(axum::middleware::from_fn_with_state(state.clone(), require_bearer))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(&cfg.bind_addr).await?;
    tracing::info!(addr = %cfg.bind_addr, "listening");
    axum::serve(listener, app).await?;
    Ok(())
}

/// Restart whatever was running before we went down. The host is a single instance, so this store
/// is the only record that a project is supposed to be up.
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
            engine_secret: rec.engine_secret,
            vault_key: rec.vault_key,
        };
        if let Err(e) = state.sandbox.start(&rec.id, &secrets).await {
            // One project failing to come back must not stop the others.
            tracing::error!(project_id = %rec.id, error = ?e, "reconcile: start failed");
        }
    }
}

/// Bearer gate for every route.
///
/// The comparison is constant-time: a byte-by-byte early exit would leak the secret one byte at a
/// time to anything that can measure response latency, which on a private network is everything.
async fn require_bearer(
    State(state): State<HostState>,
    req: Request,
    next: axum::middleware::Next,
) -> Response {
    let presented = req
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");

    if !constant_time_eq(presented.as_bytes(), state.cfg.secret.as_bytes()) {
        tracing::warn!("host request rejected: bad or missing bearer");
        return (StatusCode::UNAUTHORIZED, "unauthorized").into_response();
    }
    next.run(req).await
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

async fn healthz(State(state): State<HostState>) -> Json<serde_json::Value> {
    Json(json!({
        "ok": true,
        "sandbox_backend": match state.cfg.backend { Backend::Docker => "docker", Backend::Process => "process" },
    }))
}

#[derive(Deserialize)]
struct ProvisionBody {
    engine_secret: String,
    vault_key: String,
}

async fn provision(
    State(state): State<HostState>,
    Path(id): Path<Uuid>,
    Json(body): Json<ProvisionBody>,
) -> Result<StatusCode, HostError> {
    state
        .store
        .upsert(&id, &body.engine_secret, &body.vault_key)
        .await?;
    let secrets = Secrets {
        engine_secret: body.engine_secret,
        vault_key: body.vault_key,
    };
    state.sandbox.provision(&id, &secrets).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn status(
    State(state): State<HostState>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, HostError> {
    if state.store.get(&id).await?.is_none() {
        return Err(HostError::NotFound);
    }
    let s = state.sandbox.status(&id).await?;
    Ok(Json(json!({ "status": status_str(s) })))
}

fn status_str(s: Status) -> &'static str {
    match s {
        Status::Stopped => "stopped",
        Status::Starting => "starting",
        Status::Running => "running",
        Status::Error => "error",
    }
}

async fn start(State(state): State<HostState>, Path(id): Path<Uuid>) -> Result<StatusCode, HostError> {
    let rec = state.store.get(&id).await?.ok_or(HostError::NotFound)?;
    let secrets = Secrets {
        engine_secret: rec.engine_secret,
        vault_key: rec.vault_key,
    };
    state.sandbox.start(&id, &secrets).await?;
    state.store.set_desired_running(&id, true).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn stop(State(state): State<HostState>, Path(id): Path<Uuid>) -> Result<StatusCode, HostError> {
    state.store.get(&id).await?.ok_or(HostError::NotFound)?;
    state.sandbox.stop(&id).await?;
    state.store.set_desired_running(&id, false).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn restart(State(state): State<HostState>, Path(id): Path<Uuid>) -> Result<StatusCode, HostError> {
    let rec = state.store.get(&id).await?.ok_or(HostError::NotFound)?;
    let secrets = Secrets {
        engine_secret: rec.engine_secret,
        vault_key: rec.vault_key,
    };
    state.sandbox.restart(&id, &secrets).await?;
    state.store.set_desired_running(&id, true).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn destroy(State(state): State<HostState>, Path(id): Path<Uuid>) -> Result<StatusCode, HostError> {
    // Destroy the sandbox before forgetting it: a sandbox we have no record of is one nobody will
    // ever clean up.
    state.sandbox.destroy(&id).await?;
    state.store.delete(&id).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn proxy_engine(
    State(state): State<HostState>,
    Path((id, rest)): Path<(Uuid, String)>,
    req: Request,
) -> Result<Response, HostError> {
    let rec = state.store.get(&id).await?.ok_or(HostError::NotFound)?;
    let base = state.sandbox.engine_base(&id);
    forward(&state, req, format!("{base}/v1/{rest}"), Some(&rec.engine_secret)).await
}

async fn proxy_ingress(
    State(state): State<HostState>,
    Path((id, rest)): Path<(Uuid, String)>,
    req: Request,
) -> Result<Response, HostError> {
    let rec = state.store.get(&id).await?.ok_or(HostError::NotFound)?;
    let base = state.sandbox.engine_base(&id);
    forward(&state, req, format!("{base}/ingress/{rest}"), Some(&rec.engine_secret)).await
}

/// Forward to a project's engine, attaching that project's engine secret.
///
/// Each project has its own secret, so a bug that sent project A's request to project B's engine
/// would fail authentication rather than silently cross a tenant boundary.
async fn forward(
    state: &HostState,
    req: Request,
    upstream: String,
    bearer: Option<&str>,
) -> Result<Response, HostError> {
    let method = req.method().clone();
    let query = req.uri().query().map(|q| format!("?{q}")).unwrap_or_default();
    let mut headers = req.headers().clone();
    // The API's own credential must not be relayed into a tenant's engine.
    headers.remove(axum::http::header::AUTHORIZATION);
    headers.remove(axum::http::header::HOST);
    headers.remove(axum::http::header::CONTENT_LENGTH);

    let body = axum::body::to_bytes(req.into_body(), 16 * 1024 * 1024)
        .await
        .map_err(|_| HostError::TooLarge)?;

    let mut rb = state.http.request(method, format!("{upstream}{query}")).headers(headers).body(body);
    if let Some(b) = bearer {
        rb = rb.bearer_auth(b);
    }

    let resp = rb.send().await.map_err(|e| {
        tracing::warn!(error = ?e, "engine request failed");
        HostError::BadGateway
    })?;

    let status = resp.status();
    let mut builder = Response::builder().status(status);
    for (k, v) in resp.headers().iter() {
        if k == axum::http::header::TRANSFER_ENCODING || k == axum::http::header::CONTENT_LENGTH {
            continue;
        }
        builder = builder.header(k, v);
    }
    builder
        .body(axum::body::Body::from_stream(resp.bytes_stream()))
        .map_err(|_| HostError::Internal)
}

enum HostError {
    NotFound,
    BadGateway,
    TooLarge,
    Internal,
}

impl From<anyhow::Error> for HostError {
    fn from(e: anyhow::Error) -> Self {
        tracing::error!(error = ?e, "host operation failed");
        HostError::Internal
    }
}

impl IntoResponse for HostError {
    fn into_response(self) -> Response {
        let (code, msg) = match self {
            HostError::NotFound => (StatusCode::NOT_FOUND, "not found"),
            HostError::BadGateway => (StatusCode::BAD_GATEWAY, "engine unreachable"),
            HostError::TooLarge => (StatusCode::PAYLOAD_TOO_LARGE, "body too large"),
            HostError::Internal => (StatusCode::INTERNAL_SERVER_ERROR, "internal error"),
        };
        (code, msg).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::constant_time_eq;

    #[test]
    fn constant_time_eq_behaves_like_eq() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
        assert!(!constant_time_eq(b"", b"x"));
        assert!(constant_time_eq(b"", b""));
    }
}
