// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

//! Shared application state.

use crate::auth::jwks::JwksCache;
use crate::config::Config;
use crate::db::Db;
use crate::http::ratelimit::RateLimiter;
use crate::orchestrator::Orchestrator;
use std::sync::Arc;
use uuid::Uuid;

#[derive(Clone)]
pub struct AppState(Arc<Inner>);

pub struct Inner {
    pub cfg: Config,
    pub db: Db,
    pub jwks: JwksCache,
    /// Keys for the deployer's issuer under `AUTH_MODE=external`. A *separate* cache from `jwks`
    /// on purpose: two key sets sharing one map would let a `kid` published by one issuer satisfy
    /// a token claiming the other, which is issuer confusion delivered by a cache.
    pub external_jwks: Option<JwksCache>,
    pub http: reqwest::Client,
    pub orch: Arc<dyn Orchestrator>,
    pub ingress_limiter: RateLimiter,
    /// Fan-out of membership changes to live connections. See `membership`.
    pub membership: crate::membership::MembershipEvents,
    /// Per-project ceiling on live WebSocket bridges (ADVERSARY 011).
    pub bridges: crate::http::bridges::BridgeCounter,
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
