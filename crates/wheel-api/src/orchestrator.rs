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
}

pub mod host;

/// Used in tests, where no host service is running.
pub struct NoopOrchestrator;

#[async_trait]
impl Orchestrator for NoopOrchestrator {
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
    async fn status(&self, _: &Uuid) -> Result<ProjectStatus> {
        Ok(ProjectStatus::Stopped)
    }
}
