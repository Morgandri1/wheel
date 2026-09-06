//! Startup wiring, kept out of `main` so it can be tested.
//!
//! `main` should be the one part of a service that nothing depends on: a shell that reads the
//! environment, hands the values to these functions, and serves. Everything with a decision in it
//! — which orchestrator, which origins are allowed, what maintenance runs — lives here where a
//! test can reach it.

use crate::config::Config;
use crate::orchestrator::{host::HostClient, Orchestrator};
use crate::state::{AppState, Inner};
use anyhow::{Context, Result};
use std::sync::Arc;
use std::time::Duration;

/// Parse the CORS allowlist.
///
/// Empty entries are dropped rather than passed through: a trailing comma should not turn into an
/// origin of `""`, which is the sort of value that quietly matches something unintended.
pub fn parse_cors_origins(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

pub fn cors_origins_from_env() -> Vec<String> {
    parse_cors_origins(&std::env::var("CORS_ALLOWED_ORIGINS").unwrap_or_default())
}

/// The API owns no container runtime; every lifecycle call goes to the host.
pub fn build_orchestrator(cfg: &Config, http: reqwest::Client) -> Arc<dyn Orchestrator> {
    Arc::new(HostClient::new(
        http,
        cfg.host_url.clone(),
        cfg.host_secret.clone(),
    )) as Arc<dyn Orchestrator>
}

pub fn http_client(cfg: &Config) -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(cfg.proxy_timeout_secs))
        // Separate from the overall timeout, and much shorter, because the two describe different
        // failures. A slow *response* is normal — a project start legitimately blocks while the
        // engine comes up. A slow *connect* means the host is not there, and no amount of waiting
        // fixes it: when the host container was stopped, project creation sat for the full request
        // timeout and the platform edge returned its own 502 before we could answer. Failing the
        // connect quickly lets the handler report the outage in our own error envelope.
        .connect_timeout(Duration::from_secs(cfg.host_connect_timeout_secs))
        // Never follow a redirect on the proxy path.
        //
        // This client speaks to the host, which speaks to tenant engines, from inside a private
        // network that also holds Postgres. Following a 302 would let an upstream choose our next
        // destination — `http://postgres.railway.internal:5432`, or the host's own control plane —
        // with our credentials already attached. Relaying the response instead keeps the decision
        // with the caller and off our socket.
        //
        // It is also what the web app needs: its CSP names exactly one API origin, so a redirect
        // that reached the browser would be blocked and would surface as a silent timeout rather
        // than a refusal.
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .context("building the http client")
}

pub async fn connect_and_migrate(cfg: &Config) -> Result<sqlx::PgPool> {
    let db = sqlx::postgres::PgPoolOptions::new()
        .max_connections(10)
        .connect(&cfg.database_url)
        .await
        .context("connecting to postgres")?;
    sqlx::migrate!("./migrations")
        .run(&db)
        .await
        .context("running migrations")?;
    Ok(db)
}

pub async fn build_state(cfg: Config, db: sqlx::PgPool, http: reqwest::Client) -> AppState {
    let jwks = crate::auth::jwks::JwksCache::new(cfg.clerk_jwks_url.clone(), http.clone());
    // Priming is best-effort: the identity provider may be briefly unreachable at boot, and the
    // cache refetches on demand, so a cold start should not become an outage.
    if let Err(e) = jwks.prime().await {
        tracing::warn!(error = ?e, "could not prime JWKS at startup; will fetch on first request");
    }
    let orch = build_orchestrator(&cfg, http.clone());
    let ingress_limiter = crate::http::ratelimit::RateLimiter::new(cfg.ingress_rate_per_min);

    AppState::new(Inner {
        cfg,
        db,
        jwks,
        http,
        orch,
        ingress_limiter,
        auth_limiter: crate::http::authlimit::AuthLimiter::new(10, 50),
        engine_base_override: None,
    })
}

/// Reclaim expired rate-limit windows and spent websocket tickets.
///
/// Safe to run in every replica: both sweeps are idempotent deletes of already-expired rows, so
/// concurrent runs cost a little duplicated work and nothing else. No leader election needed.
pub async fn run_maintenance_once(db: &sqlx::PgPool) {
    if let Err(e) = crate::http::ratelimit::sweep(db).await {
        tracing::warn!(error = ?e, "rate limit sweep failed");
    }
    if let Err(e) = crate::routes::ws_ticket::sweep(db).await {
        tracing::warn!(error = ?e, "ws ticket sweep failed");
    }
}

pub fn spawn_maintenance(db: sqlx::PgPool, every: Duration) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(every);
        loop {
            tick.tick().await;
            run_maintenance_once(&db).await;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cors_origins_parse() {
        assert_eq!(
            parse_cors_origins("https://wheel.dev,https://www.wheel.dev"),
            vec!["https://wheel.dev", "https://www.wheel.dev"]
        );
        // Whitespace and stray commas are operator typos, not origins.
        assert_eq!(
            parse_cors_origins(" https://a.test , , https://b.test ,"),
            vec!["https://a.test", "https://b.test"]
        );
        assert!(parse_cors_origins("").is_empty());
        assert!(parse_cors_origins(",,,").is_empty());
    }
}
