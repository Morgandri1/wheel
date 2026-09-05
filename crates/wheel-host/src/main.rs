//! `wheel-host` — the sandbox supervisor (ARCHITECTURE §4b).
//!
//! Runs on the single engine machine, one instance only. Owns every project's sandbox and is the
//! only process that holds engine secrets at runtime. It has no public domain: the API reaches it
//! over private networking with a bearer secret, and nothing else may.
//!
//! Thin by design — the wiring with decisions in it lives in the library, where tests can reach it.

use anyhow::{Context, Result};
use wheel_host::config::Config;
use wheel_host::{build_router, build_state, reconcile_on_boot};

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

    let bind = cfg.bind_addr.clone();
    let state = build_state(cfg)?;

    reconcile_on_boot(&state).await;

    let app = build_router(state);
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    tracing::info!(addr = %bind, "listening");
    axum::serve(listener, app).await?;
    Ok(())
}
