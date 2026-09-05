//! Shared application state.

use crate::auth::jwks::JwksCache;
use crate::config::Config;
use crate::http::ratelimit::RateLimiter;
use crate::orchestrator::Orchestrator;
use std::sync::Arc;
use uuid::Uuid;

#[derive(Clone)]
pub struct AppState(Arc<Inner>);

pub struct Inner {
    pub cfg: Config,
    pub db: sqlx::PgPool,
    pub jwks: JwksCache,
    pub http: reqwest::Client,
    pub orch: Arc<dyn Orchestrator>,
    pub ingress_limiter: RateLimiter,
    pub auth_limiter: crate::http::authlimit::AuthLimiter,
    /// Test hook: when set, every project's engine resolves to this base URL instead of the
    /// docker-network hostname. Only ever populated by the test harness.
    pub engine_base_override: Option<String>,
}

impl std::ops::Deref for AppState {
    type Target = Inner;
    fn deref(&self) -> &Inner {
        &self.0
    }
}

impl AppState {
    pub fn new(inner: Inner) -> Self {
        AppState(Arc::new(inner))
    }

    /// Base URL for proxying to a project's engine, via the host.
    pub fn engine_base_url(&self, project_id: &Uuid) -> String {
        match &self.engine_base_override {
            Some(base) => base.trim_end_matches('/').to_string(),
            None => self.cfg.host_engine_url(project_id),
        }
    }

    /// Base URL for proxying a public ingress hit, via the host.
    pub fn ingress_base_url(&self, project_id: &Uuid) -> String {
        match &self.engine_base_override {
            Some(base) => format!("{}/ingress", base.trim_end_matches('/')),
            None => self.cfg.host_ingress_url(project_id),
        }
    }
}
