//! `wheel-host` binary. All logic lives in the library so it can be tested; this only wires
//! configuration, logging and the listener.

use anyhow::{Context, Result};
use std::sync::Arc;
use wheel_host::config::{Backend, Config};
use wheel_host::sandbox::Sandbox;
use wheel_host::{build_router, reconcile_on_boot, store, HostState};

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
        Backend::Docker => Arc::new(wheel_host::sandbox::docker::DockerSandbox::connect(
            cfg.clone(),
        )?),
        Backend::External => Arc::new(wheel_host::sandbox::external::ExternalSandbox::new(
            cfg.engine_base_url.clone(),
        )),
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

    let app = build_router(state.clone());

    let listener = tokio::net::TcpListener::bind(&cfg.bind_addr).await?;
    tracing::info!(addr = %cfg.bind_addr, "listening");
    axum::serve(listener, app).await?;
    Ok(())
}
