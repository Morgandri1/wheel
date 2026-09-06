//! Container lifecycle, behind a trait.
//!
//! The trait exists because *where* this code runs is still being decided: originally the API
//! mounted the docker socket itself, but with the API running as N stateless replicas the socket
//! moves to a single host service and the API drives it over HTTP. Both shapes satisfy this
//! interface, so the routes are written against the trait and neither ruling forces a rewrite.

use crate::crypto::Secret;
use crate::models::ProjectStatus;
use anyhow::Result;
use async_trait::async_trait;
use uuid::Uuid;

pub struct EngineSecrets {
    pub engine_secret: Secret,
    pub vault_key: Secret,
}

#[async_trait]
pub trait Orchestrator: Send + Sync {
    /// Create the volume and container for a project if they do not already exist. Idempotent.
    async fn provision(&self, project_id: &Uuid, secrets: &EngineSecrets) -> Result<()>;
    async fn start(&self, project_id: &Uuid) -> Result<()>;
    async fn stop(&self, project_id: &Uuid) -> Result<()>;
    async fn restart(&self, project_id: &Uuid) -> Result<()>;
    /// Remove container *and* volume. Idempotent.
    async fn destroy(&self, project_id: &Uuid) -> Result<()>;
    /// Observed status, from the runtime rather than from our database.
    async fn status(&self, project_id: &Uuid) -> Result<ProjectStatus>;

    /// Is the sandbox host reachable and serving?
    ///
    /// Liveness of the host itself rather than of any project, for a deploy gate: the host has no
    /// public domain, so nothing outside this API can ask it directly.
    async fn host_alive(&self) -> Result<()>;
}

/// A refusal the caller can act on, as opposed to a failure they cannot.
///
/// Most host errors are ours to fix and read as `internal`. This one is not: the machine is out of
/// disk, the host says so precisely, and flattening that into "an unexpected error occurred" is how
/// a full volume cost us an afternoon. Carried through `anyhow` so the trait stays simple; the
/// route recovers it with `downcast_ref`.
#[derive(Debug, thiserror::Error)]
pub enum HostRefusal {
    #[error("the host has no room to start a project")]
    OutOfDisk,
}

pub mod host;

/// Used in tests, where no host service is running.
pub struct NoopOrchestrator;

#[async_trait]
impl Orchestrator for NoopOrchestrator {
    async fn host_alive(&self) -> Result<()> {
        Ok(())
    }
    async fn provision(&self, _: &Uuid, _: &EngineSecrets) -> Result<()> {
        Ok(())
    }
    async fn start(&self, _: &Uuid) -> Result<()> {
        Ok(())
    }
    async fn stop(&self, _: &Uuid) -> Result<()> {
        Ok(())
    }
    async fn restart(&self, _: &Uuid) -> Result<()> {
        Ok(())
    }
    async fn destroy(&self, _: &Uuid) -> Result<()> {
        Ok(())
    }
    /// It stands in for a host on which everything works. Reporting `stopped` after a start it just
    /// accepted would make it a fake of a broken host instead.
    async fn status(&self, _: &Uuid) -> Result<ProjectStatus> {
        Ok(ProjectStatus::Running)
    }
}
