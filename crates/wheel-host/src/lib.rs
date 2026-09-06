//! `wheel-host` — the sandbox supervisor (ARCHITECTURE §4b).
//!
//! Runs on the single engine machine, one instance only. Owns every project's sandbox and is the
//! only process that holds engine secrets at runtime. It has no public domain: the API reaches it
//! over private networking with a bearer secret, and nothing else may.
//!
//! The security posture is blunt on purpose. Anything that can reach this port can control every
//! tenant's sandbox, so the bearer check is the first thing in the stack and applies to every
//! route including the proxies.

pub mod auth_limit;
pub mod config;
pub mod proxy;
pub mod sandbox;
pub mod store;
pub mod vault_key;

use anyhow::Context as _;
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
pub struct HostState {
    pub cfg: Config,
    pub sandbox: Arc<dyn Sandbox>,
    pub store: Arc<store::Store>,
    pub http: reqwest::Client,
    /// Throttles failed bearer attempts per peer (ADVERSARY: :7100 needs a rate limit as well as
    /// a constant-time compare).
    pub auth_limiter: Arc<auth_limit::AuthLimiter>,
    pub ready: Readiness,
}

/// Whether the host will answer for the projects it owns yet.
///
/// The host has to answer the platform's health check within seconds of starting, but it must not
/// serve project routes while it is still working out which sandboxes it owns — a request answered
/// mid-reconcile sees a host that has forgotten them. So liveness is immediate and project routes
/// are 503 until reconcile finishes.
#[derive(Clone)]
pub struct Readiness(Arc<std::sync::atomic::AtomicBool>);

impl Readiness {
    /// For a host whose projects are already accounted for: an embedder that reconciled before
    /// building it, or a test with nothing to reconcile.
    pub fn serving_from_start() -> Self {
        Self(Arc::new(std::sync::atomic::AtomicBool::new(true)))
    }

    /// For a host that will reconcile behind its own listener, and calls `open` when it has.
    pub fn serving_after_reconcile() -> Self {
        Self(Arc::new(std::sync::atomic::AtomicBool::new(false)))
    }

    pub fn open(&self) {
        self.0.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn is_open(&self) -> bool {
        self.0.load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// Build the sandbox backend named by configuration.
///
/// Kept in the library rather than in `main` so the selection — including the refusal to start on
/// a backend that is not implemented — is testable. Starting with a backend that silently does
/// nothing would look healthy while every project stayed dead.
pub fn build_sandbox(
    cfg: &config::Config,
    store: Arc<store::Store>,
) -> anyhow::Result<Arc<dyn Sandbox>> {
    Ok(match cfg.backend {
        config::Backend::Docker => Arc::new(sandbox::docker::DockerSandbox::connect(cfg.clone())?),
        config::Backend::External => Arc::new(sandbox::external::ExternalSandbox::new(
            cfg.engine_base_url.clone(),
        )),
        // The process backend allocates a uid per project, so it needs the durable store the
        // allocation lives in — a uid derived fresh each boot would move a project's files.
        config::Backend::Process => {
            Arc::new(sandbox::process::ProcessSandbox::new(cfg.clone(), store))
        }
    })
}

/// Assemble the running state: sandbox backend, durable store, and an http client for the proxy.
pub fn build_state(cfg: config::Config) -> anyhow::Result<HostState> {
    let store = Arc::new(store::Store::open(&format!("{}/host.db", cfg.data_dir))?);
    let sandbox = build_sandbox(&cfg, store.clone())?;
    Ok(HostState {
        cfg,
        sandbox,
        store,
        // Same reasoning as the API's client: this one reaches tenant engines from a network
        // that also holds Postgres, so an upstream redirect must never choose our next hop.
        http: reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .context("building the engine http client")?,
        auth_limiter: std::sync::Arc::new(auth_limit::AuthLimiter::new(cfg_auth_failure_budget())),
        ready: Readiness::serving_after_reconcile(),
    })
}

/// Restart whatever was running before we went down.
///
/// The host is deliberately a single instance, so nothing else will notice that a project's engine
/// died with us. Without this, a host restart silently stops every tenant's agents.
pub async fn reconcile_on_boot(state: &HostState) {
    let wanted = match state.store.all_desired_running().await {
        Ok(w) => w,
        Err(e) => {
            tracing::error!(error = ?e, "could not read desired state; skipping reconcile");
            return;
        }
    };
    tracing::info!(
        count = wanted.len(),
        "reconciling projects that were running"
    );
    for rec in wanted {
        // Rows written before canonicalisation existed still hold an unusable spelling, and boot is
        // the only moment we look at every project. Rewrite them now, so the next restart is clean
        // and the engine we are about to start gets a key it can decode.
        let vault_key = vault_key::canonical_or_passthrough(&rec.vault_key);
        if vault_key != rec.vault_key {
            tracing::info!(project = %rec.id, "rewriting a vault key the engine could not decode");
            if let Err(e) = state
                .store
                .upsert(&rec.id, &rec.engine_secret, &vault_key)
                .await
            {
                tracing::error!(project = %rec.id, error = ?e, "could not persist the corrected vault key");
            }
        }
        let secrets = Secrets {
            engine_secret: rec.engine_secret.clone(),
            vault_key,
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
pub async fn require_bearer(State(state): State<HostState>, req: Request, next: Next) -> Response {
    // Peer address, when the server was built with connect info. Absent in unit tests, which
    // exercise the router directly; those fall back to a fixed key so the limiter still applies.
    let peer = req
        .extensions()
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        .map(|ci| ci.0.ip())
        .unwrap_or(std::net::IpAddr::from([0, 0, 0, 0]));

    // Refuse before comparing. Past its budget, a peer learns nothing further — not even the
    // timing of a comparison.
    if !state.auth_limiter.may_attempt(peer) {
        tracing::warn!(%peer, "refusing bearer attempt: peer is over its failure budget");
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(
                json!({"error": {"code": "rate_limited", "message": "Too many failed attempts."}}),
            ),
        )
            .into_response();
    }

    let presented = req
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");

    if !constant_time_eq(presented.as_bytes(), state.cfg.secret.as_bytes()) {
        state.auth_limiter.record_failure(peer);
        tracing::warn!(%peer, "rejected host request with bad or missing bearer");
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": {"code": "unauthorized", "message": "Unauthorized."}})),
        )
            .into_response();
    }
    next.run(req).await
}

/// Failed bearer attempts allowed per peer per minute before the host stops answering them.
///
/// Generous by default: the legitimate caller is a single API that authenticates correctly every
/// time, so this budget is only ever consumed by something guessing.
fn cfg_auth_failure_budget() -> u32 {
    std::env::var("HOST_AUTH_FAILURE_BUDGET")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(30)
}

pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Unauthenticated liveness, for the platform's health checker.
///
/// A platform health check cannot present the host bearer, so putting the whole router behind
/// `require_bearer` means every check answers 401 and the platform stops the container — which is
/// exactly what happened: the host went down and every project create hung on an unreachable host.
/// This says only that the process is up. Anything that describes the host — backend, project
/// counts — stays behind the bearer on `/host/v1/healthz`.
pub async fn liveness() -> Json<serde_json::Value> {
    Json(json!({ "ok": true }))
}

pub async fn healthz(State(state): State<HostState>) -> Json<serde_json::Value> {
    Json(json!({
        "ok": true,
        "sandbox_backend": match state.cfg.backend {
            Backend::Docker => "docker",
            Backend::Process => "process",
            Backend::External => "external",
        },
    }))
}

#[derive(Deserialize)]
pub struct PutProject {
    engine_secret: String,
    vault_key: String,
}

/// Create-or-update a project's sandbox record. Idempotent, per §4b.
pub async fn put_project(
    State(state): State<HostState>,
    Path(id): Path<Uuid>,
    Json(body): Json<PutProject>,
) -> Response {
    // Canonicalise once, here, and store that. The engine decodes the vault key with the standard
    // base64 alphabet; storing whatever spelling arrived means a host restart reconciles every
    // engine with a key it cannot use, and the API's own fix-up would only reach projects that get
    // started through it again.
    let vault_key = vault_key::canonical_or_passthrough(&body.vault_key);
    if let Err(e) = state
        .store
        .upsert(&id, &body.engine_secret, &vault_key)
        .await
    {
        return internal(e, "storing project record");
    }
    let secrets = Secrets {
        engine_secret: body.engine_secret,
        vault_key,
    };
    match state.sandbox.provision(&id, &secrets).await {
        Ok(()) => (StatusCode::OK, Json(json!({"ok": true}))).into_response(),
        Err(e) => internal(e, "provisioning sandbox"),
    }
}

pub async fn get_project(State(state): State<HostState>, Path(id): Path<Uuid>) -> Response {
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

pub async fn start(State(state): State<HostState>, Path(id): Path<Uuid>) -> Response {
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

pub async fn stop(State(state): State<HostState>, Path(id): Path<Uuid>) -> Response {
    if let Err(e) = state.store.set_desired_running(&id, false).await {
        return internal(e, "persisting desired state");
    }
    match state.sandbox.stop(&id).await {
        Ok(()) => (StatusCode::OK, Json(json!({"status": "stopped"}))).into_response(),
        Err(e) => internal(e, "stopping sandbox"),
    }
}

pub async fn restart(State(state): State<HostState>, Path(id): Path<Uuid>) -> Response {
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

pub async fn delete_project(State(state): State<HostState>, Path(id): Path<Uuid>) -> Response {
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

/// A 500 in the shape every Wheel service renders errors in.
///
/// The uniform envelope matters here specifically because the API relays this body to the user: a
/// bare `{"error":"internal"}` from one route and `{"error":{"code","message"}}` from the proxy
/// means a client cannot read a host failure without knowing which code path produced it.
///
/// The cause is logged and never returned. Storage and orchestration details are operator
/// information, and `what` is written for an operator reading logs, not for a tenant.
pub fn internal(e: anyhow::Error, what: &str) -> Response {
    tracing::error!(error = %format_args!("{e:#}"), "{what} failed");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({
            "error": {
                "code": "internal",
                "message": format!("The host could not complete this request while {what}."),
            }
        })),
    )
        .into_response()
}

/// Bind and serve until the process is asked to stop.
///
/// The binary is a wrapper around this, so the boot sequence — reconcile before accepting traffic,
/// connect info for the bearer limiter — has one implementation that tests can drive rather than
/// a second copy in `main` that only ever runs in production.
pub async fn serve(cfg: config::Config) -> anyhow::Result<()> {
    let bind = cfg.bind_addr.clone();
    let state = build_state(cfg)?;

    // Listen first. Reconciling fourteen projects takes longer than the platform's health-check
    // window, and a host that is stopped for failing that check is a host that never reconciles at
    // all. Project routes stay 503 until `ready` flips, so nothing is served from a half-restored
    // view; only liveness answers early, which is all the checker asks.
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    tracing::info!(addr = %bind, "listening");

    let reconciling = state.clone();
    tokio::spawn(async move {
        reconcile_on_boot(&reconciling).await;
        reconciling.ready.open();
        tracing::info!("reconcile complete; project routes are open");
    });

    serve_on(listener, state).await
}

/// Serve an already-bound listener. Split out so a test can bind port 0 and drive the real router.
pub async fn serve_on(listener: tokio::net::TcpListener, state: HostState) -> anyhow::Result<()> {
    // With connect info, so the failed-bearer limiter can key on the peer address rather than
    // throttling every caller together.
    axum::serve(
        listener,
        build_router(state).into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await?;
    Ok(())
}

/// Refuse project routes until boot reconciliation has finished.
///
/// A 503 with `Retry-After` is the honest answer: the host is up, it does not yet know which
/// sandboxes it owns, and the caller should come back. Answering them anyway would report projects
/// as stopped that are in fact running, and the API would faithfully relay that to the user.
async fn require_ready(
    State(state): State<HostState>,
    req: axum::extract::Request,
    next: Next,
) -> Response {
    if state.ready.is_open() {
        return next.run(req).await;
    }
    (
        StatusCode::SERVICE_UNAVAILABLE,
        [("retry-after", "2")],
        Json(json!({
            "error": {
                "code": "starting",
                "message": "The host is still restoring projects from its last run.",
            }
        })),
    )
        .into_response()
}

/// Assemble the host router. Split out of `main` so tests can drive the real routes — including
/// the bearer middleware — without binding a port or owning a container runtime.
pub fn build_router(state: HostState) -> Router {
    Router::new()
        .route("/host/v1/healthz", get(healthz))
        .route(
            "/host/v1/projects/{id}",
            put(put_project).get(get_project).delete(delete_project),
        )
        .route("/host/v1/projects/{id}/start", axum::routing::post(start))
        .route("/host/v1/projects/{id}/stop", axum::routing::post(stop))
        .route(
            "/host/v1/projects/{id}/restart",
            axum::routing::post(restart),
        )
        .route("/host/v1/projects/{id}/engine/{*rest}", any(proxy::engine))
        .route(
            "/host/v1/projects/{id}/ingress/{*rest}",
            any(proxy::ingress),
        )
        // Ready-gate inside the bearer layer: an unauthenticated caller learns nothing about
        // whether we are still starting, because it never gets past the bearer.
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            require_ready,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            require_bearer,
        ))
        // Merged AFTER the layer, so it is the one route the bearer middleware does not cover.
        .merge(Router::new().route("/healthz", get(liveness)))
        .with_state(state)
}
