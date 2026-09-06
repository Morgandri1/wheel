//! `wheel-host` — the sandbox supervisor (ARCHITECTURE §4b).
//!
//! Runs on the single engine machine, one instance only. Owns every project's sandbox and is the
//! only process that holds engine secrets at runtime. It has no public domain: the API reaches it
//! over private networking with a bearer secret, and nothing else may.
//!
//! A wrapper around `wheel_host::serve`, so the boot sequence has one implementation and tests can
//! reach it.

use anyhow::{Context, Result};
use wheel_host::config::Config;

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
    wheel_host::serve(cfg).await
}
