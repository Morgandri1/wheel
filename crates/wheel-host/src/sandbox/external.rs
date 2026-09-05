//! Dev-only sandbox backend: an engine that someone else already started.
//!
//! Exists for one reason. On macOS the docker bridge network is not reachable from the host, so a
//! natively-run `wheel-host` cannot resolve `wheel-p-<id>:7000`. That makes the containerised
//! stack the only way to exercise the chain locally, and a from-scratch Rust image build is slow
//! enough to be a bad inner loop.
//!
//! With this backend the engine runs as an ordinary local process and the host talks to it over
//! loopback, so the API -> host -> engine path — including engine-secret injection and the
//! ownership checks in front of it — is exercised for real.
//!
//! What it deliberately does NOT cover: container lifecycle. Provision/start/stop are no-ops
//! because the engine's existence is somebody else's problem here. Only the docker and process
//! backends prove lifecycle, and this one refuses to load outside dev.

use super::{Sandbox, Secrets, Status};
use anyhow::Result;
use async_trait::async_trait;
use uuid::Uuid;

pub struct ExternalSandbox {
    base: String,
    http: reqwest::Client,
}

impl ExternalSandbox {
    pub fn new(base: String) -> Self {
        Self {
            base: base.trim_end_matches('/').to_string(),
            http: reqwest::Client::new(),
        }
    }
}

#[async_trait]
impl Sandbox for ExternalSandbox {
    async fn provision(&self, _: &Uuid, _: &Secrets) -> Result<()> {
        Ok(())
    }
    async fn start(&self, _: &Uuid, _: &Secrets) -> Result<()> {
        Ok(())
    }
    async fn stop(&self, _: &Uuid) -> Result<()> {
        Ok(())
    }
    async fn restart(&self, _: &Uuid, _: &Secrets) -> Result<()> {
        Ok(())
    }
    async fn destroy(&self, _: &Uuid) -> Result<()> {
        Ok(())
    }

    /// Status is the engine's own readiness, probed the same way the docker backend probes it.
    async fn status(&self, _: &Uuid) -> Result<Status> {
        let url = format!("{}/healthz", self.base);
        match self.http.get(&url).send().await {
            Ok(r) if r.status().is_success() => Ok(Status::Running),
            _ => Ok(Status::Stopped),
        }
    }

    fn engine_base(&self, _: &Uuid) -> String {
        self.base.clone()
    }
}
