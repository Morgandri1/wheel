use anyhow::{Context, Result};
use std::sync::Arc;
use wheel_api::config::Config;
use wheel_api::state::{AppState, Inner};

#[tokio::main]
async fn main() -> Result<()> {
    // JSON logs, so the request-id/field structure survives into the log aggregator.
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,wheel_api=debug".into()),
        )
        .init();

    // Config validation happens before anything binds a port or opens a socket: an unsafe
    // configuration must fail as a startup crash, not as a subtly-permissive running service.
    let cfg = Config::from_env().context("configuration")?;
    tracing::info!(env = ?cfg.env, "wheel-api starting");

    let db = sqlx::postgres::PgPoolOptions::new()
        .max_connections(10)
        .connect(&cfg.database_url)
        .await
        .context("connecting to postgres")?;
    sqlx::migrate!("./migrations")
        .run(&db)
        .await
        .context("running migrations")?;

    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(cfg.proxy_timeout_secs))
        .build()?;

    let jwks = wheel_api::auth::jwks::JwksCache::new(cfg.clerk_jwks_url.clone(), http.clone());
    if let Err(e) = jwks.prime().await {
        // Not fatal: Clerk may be briefly unreachable at boot, and the cache refetches on demand.
        tracing::warn!(error = ?e, "could not prime JWKS at startup; will fetch on first request");
    }

    let orch = build_orchestrator(&cfg, http.clone())?;
    let limiter = wheel_api::http::ratelimit::RateLimiter::new(cfg.ingress_rate_per_min);
    let origins: Vec<String> = std::env::var("CORS_ALLOWED_ORIGINS")
        .unwrap_or_default()
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    let bind = cfg.bind_addr.clone();
    let state = AppState::new(Inner {
        cfg,
        db: db.clone(),
        jwks,
        http,
        orch,
        ingress_limiter: limiter,
        engine_base_override: None,
    });

    // Periodic maintenance. Safe to run in every replica: the sweep is an idempotent DELETE of
    // expired rows, so concurrent runs cost a little duplicated work and nothing else.
    tokio::spawn({
        let db = db.clone();
        async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(60));
            loop {
                tick.tick().await;
                if let Err(e) = wheel_api::http::ratelimit::sweep(&db).await {
                    tracing::warn!(error = ?e, "rate limit sweep failed");
                }
            }
        }
    });

    let app = wheel_api::build_router(state, &origins);
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    tracing::info!(%bind, "listening");
    axum::serve(listener, app).await?;
    Ok(())
}

fn build_orchestrator(
    cfg: &Config,
    http: reqwest::Client,
) -> Result<Arc<dyn wheel_api::orchestrator::Orchestrator>> {
    use wheel_api::orchestrator::{host::HostClient, Orchestrator};
    Ok(Arc::new(HostClient::new(
        http,
        cfg.host_url.clone(),
        cfg.host_secret.clone(),
    )) as Arc<dyn Orchestrator>)
}
