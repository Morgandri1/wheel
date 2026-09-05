//! Entry point. Deliberately thin: everything with a decision in it lives in `wheel_api::boot`,
//! where it can be tested.

use anyhow::{Context, Result};
use wheel_api::boot;
use wheel_api::config::Config;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,wheel_api=debug".into()),
        )
        .init();

    // Config validation happens before anything binds a port: an unsafe configuration must fail as
    // a startup crash, not as a subtly-permissive running service.
    let cfg = Config::from_env().context("configuration")?;
    tracing::info!(env = ?cfg.env, "wheel-api starting");

    let http = boot::http_client(&cfg)?;
    let db = boot::connect_and_migrate(&cfg).await?;
    let origins = boot::cors_origins_from_env();
    let bind = cfg.bind_addr.clone();

    let state = boot::build_state(cfg, db.clone(), http).await;
    boot::spawn_maintenance(db, std::time::Duration::from_secs(60));

    let app = wheel_api::build_router(state, &origins);
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    tracing::info!(%bind, "listening");
    axum::serve(listener, app).await?;
    Ok(())
}
