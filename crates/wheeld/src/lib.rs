//! `wheeld` — api, host and engines in one executable (ARCHITECTURE M1.7).
//!
//! Wheel in production is three services on two machines. That is the right shape for multi-tenant
//! cloud and the wrong shape for someone who wants to try it, or hack on it, on a laptop. `wheeld`
//! runs the same code in one process with no configuration.
//!
//! It is a composition, not a fourth implementation. The API router, the host router and the engine
//! are the ones that ship; what changes is only how they are wired together.

pub mod config;
pub mod embedded;
pub mod supervise;

pub use config::Settings;

use anyhow::{Context, Result};
use std::sync::Arc;

/// Boot the whole product in this process.
///
/// The order is the contract: the host must be listening and its projects reconciled before the API
/// can provision anything, and the API must know the host's address, which is only assigned once
/// its listener is bound. So the host listener comes first, then the environment the API reads.
pub async fn run(settings: Settings) -> Result<()> {
    let data_dir = supervise::prepare_data_dir(&settings.data_dir)?;
    let keys = supervise::Keys::load_or_create(&data_dir)?;

    // Loopback only, on a port the OS picks. Nothing outside this machine may reach the host: it
    // is the half of the process that can start and stop any project's engine.
    let host_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .context("binding the host listener")?;
    let host_addr = host_listener.local_addr()?;
    let host_url = format!("http://{host_addr}");

    supervise::apply_defaults(&supervise::composed_env(&data_dir, &keys, &host_url));

    let host_state = build_host_state(&data_dir)?;
    wheel_host::reconcile_on_boot(&host_state).await;
    tokio::spawn(async move {
        if let Err(e) = wheel_host::serve_on(host_listener, host_state).await {
            tracing::error!(error = %format_args!("{e:#}"), "the sandbox host stopped");
        }
    });
    tracing::info!(%host_url, "sandbox host ready");

    serve_api(&settings.bind).await
}

/// The host, with engines embedded rather than spawned.
fn build_host_state(data_dir: &std::path::Path) -> Result<wheel_host::HostState> {
    let cfg = wheel_host::config::Config::from_env().context("host configuration")?;
    let store = Arc::new(wheel_host::store::Store::open(
        &data_dir.join("host.db").display().to_string(),
    )?);
    let sandbox = Arc::new(embedded::EmbeddedSandbox::new(
        data_dir.to_path_buf(),
        data_dir.join("run"),
        std::time::Duration::from_secs(cfg.start_timeout_secs),
    ));
    Ok(wheel_host::HostState {
        cfg,
        sandbox,
        store,
        http: reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .context("building the host http client")?,
        auth_limiter: Arc::new(wheel_host::auth_limit::AuthLimiter::new(30)),
    })
}

async fn serve_api(bind: &str) -> Result<()> {
    let cfg = wheel_api::config::Config::from_env().context("api configuration")?;
    let http = wheel_api::boot::http_client(&cfg)?;
    let db = wheel_api::boot::connect_and_migrate(&cfg).await?;
    let origins = wheel_api::boot::cors_origins_from_env();
    let state = wheel_api::boot::build_state(cfg, db.clone(), http).await;
    wheel_api::boot::spawn_maintenance(db, std::time::Duration::from_secs(60));

    let app = wheel_api::build_router(state, &origins);
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .with_context(|| format!("binding {bind}"))?;
    tracing::info!(%bind, "wheel is ready — open http://localhost:8080");
    axum::serve(listener, app).await?;
    Ok(())
}
