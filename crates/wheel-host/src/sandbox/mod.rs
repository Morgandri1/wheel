//! The `Sandbox` abstraction.
//!
//! Two backends satisfy it — `docker` (local dev, any VM with a daemon) and `process` (production
//! on Railway, where there is no docker daemon). Nothing above the host knows which is in use;
//! that is the whole point of the trait, and it is why the API can be deployed unchanged against
//! either.

pub mod docker;

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Stopped,
    Starting,
    Running,
    Error,
}

/// Secrets for one project's engine. Held only in memory and in the host's own store — they are
/// never returned by any host endpoint.
#[derive(Clone)]
pub struct Secrets {
    pub engine_secret: String,
    pub vault_key: String,
}

impl std::fmt::Debug for Secrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secrets(<redacted>)")
    }
}

#[async_trait]
pub trait Sandbox: Send + Sync {
    /// Create-or-update the sandbox for a project. Idempotent.
    async fn provision(&self, id: &Uuid, secrets: &Secrets) -> Result<()>;
    /// Start, blocking until the engine reports healthy or the timeout elapses.
    async fn start(&self, id: &Uuid, secrets: &Secrets) -> Result<()>;
    async fn stop(&self, id: &Uuid) -> Result<()>;
    async fn restart(&self, id: &Uuid, secrets: &Secrets) -> Result<()>;
    /// Stop and destroy the sandbox and its data. Idempotent.
    async fn destroy(&self, id: &Uuid) -> Result<()>;
    async fn status(&self, id: &Uuid) -> Result<Status>;
    /// Base URL of this project's engine control plane, for the host's proxy.
    fn engine_base(&self, id: &Uuid) -> String;
}
