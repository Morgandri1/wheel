//! Client for `wheel-host` (ARCHITECTURE §4b).
//!
//! The API is a stateless gateway: it owns no container runtime and never speaks to an engine
//! directly. Every lifecycle operation is an authenticated call to the single host machine, which
//! is the only process that holds engine secrets at runtime.
//!
//! `WHEEL_HOST_SECRET` authenticates *the API* to the host. It is never derived from, mixed with,
//! or exposed to anything a client sends, and it never appears in a response or a log line.

use super::{EngineSecrets, Orchestrator};
use crate::crypto::Secret;
use crate::models::ProjectStatus;
use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;
use std::time::Duration;
use uuid::Uuid;

pub struct HostClient {
    http: reqwest::Client,
    base: String,
    secret: Secret,
}

#[derive(Deserialize)]
struct HostProjectStatus {
    status: String,
    #[serde(default)]
    last_error: Option<String>,
}

impl HostClient {
    pub fn new(http: reqwest::Client, base: String, secret: Secret) -> Self {
        Self {
            http,
            base: base.trim_end_matches('/').to_string(),
            secret,
        }
    }

    fn url(&self, project_id: &Uuid, suffix: &str) -> String {
        format!("{}/host/v1/projects/{}{}", self.base, project_id, suffix)
    }

    fn req(&self, method: reqwest::Method, url: String) -> reqwest::RequestBuilder {
        self.http
            .request(method, url)
            .bearer_auth(self.secret.expose())
    }

    /// Retry with jittered backoff. Only ever called for idempotent operations — a retried
    /// non-idempotent call could double-create or double-charge, so those get exactly one attempt.
    async fn with_retry<F, Fut, T>(&self, attempts: u32, f: F) -> Result<T>
    where
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = Result<T>>,
    {
        let mut last: Option<anyhow::Error> = None;
        for i in 0..attempts {
            match f().await {
                Ok(v) => return Ok(v),
                Err(e) => {
                    if i + 1 < attempts {
                        // Jitter so N replicas retrying the same failed host don't synchronise into
                        // a thundering herd.
                        let base_ms = 100u64 << i;
                        let jitter = fastrand_ms(base_ms / 2);
                        tokio::time::sleep(Duration::from_millis(base_ms + jitter)).await;
                    }
                    last = Some(e);
                }
            }
        }
        Err(last.unwrap_or_else(|| anyhow::anyhow!("no attempts made")))
    }
}

/// Small dependency-free jitter source; nothing here needs cryptographic randomness.
fn fastrand_ms(max: u64) -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    if max == 0 {
        return 0;
    }
    let n = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    n % max
}

#[async_trait]
impl Orchestrator for HostClient {
    async fn host_alive(&self) -> Result<()> {
        // The host's unauthenticated liveness route, not the bearer-gated `/host/v1/healthz`: this
        // asks only "is the process serving", and the bearer is still sent because everything else
        // on that port requires it and a probe that behaves differently is a probe of a different
        // thing.
        let r = self
            .req(reqwest::Method::GET, format!("{}/healthz", self.base))
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .context("host: liveness request failed")?;
        if r.status().is_success() {
            Ok(())
        } else {
            bail!("host answered {} to a liveness probe", r.status())
        }
    }

    async fn provision(&self, project_id: &Uuid, secrets: &EngineSecrets) -> Result<()> {
        // PUT is idempotent by contract, so it is safe to retry.
        self.with_retry(3, || async {
            let r = self
                .req(reqwest::Method::PUT, self.url(project_id, ""))
                .json(&json!({
                    "engine_secret": secrets.engine_secret.expose(),
                    "vault_key": secrets.vault_key.expose(),
                    "capabilities": { "http": false },
                }))
                .send()
                .await
                .context("host: provision request failed")?;
            ensure_ok(r).await
        })
        .await
    }

    async fn start(&self, project_id: &Uuid) -> Result<()> {
        // The host blocks until the engine's /healthz is green, so this can legitimately take
        // tens of seconds; it is idempotent, so a retry is safe.
        self.with_retry(2, || async {
            let r = self
                .req(reqwest::Method::POST, self.url(project_id, "/start"))
                .timeout(Duration::from_secs(35))
                .send()
                .await
                .context("host: start request failed")?;
            ensure_ok(r).await
        })
        .await
    }

    async fn stop(&self, project_id: &Uuid) -> Result<()> {
        let r = self
            .req(reqwest::Method::POST, self.url(project_id, "/stop"))
            .send()
            .await
            .context("host: stop request failed")?;
        ensure_ok(r).await
    }

    async fn restart(&self, project_id: &Uuid) -> Result<()> {
        let r = self
            .req(reqwest::Method::POST, self.url(project_id, "/restart"))
            .timeout(Duration::from_secs(35))
            .send()
            .await
            .context("host: restart request failed")?;
        ensure_ok(r).await
    }

    async fn destroy(&self, project_id: &Uuid) -> Result<()> {
        self.with_retry(3, || async {
            let r = self
                .req(reqwest::Method::DELETE, self.url(project_id, ""))
                .send()
                .await
                .context("host: destroy request failed")?;
            // A sandbox that is already gone is a success, not a failure: delete must converge.
            if r.status() == reqwest::StatusCode::NOT_FOUND {
                return Ok(());
            }
            ensure_ok(r).await
        })
        .await
    }

    async fn status(&self, project_id: &Uuid) -> Result<ProjectStatus> {
        let r = self
            .req(reqwest::Method::GET, self.url(project_id, ""))
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .context("host: status request failed")?;

        if r.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(ProjectStatus::Stopped);
        }
        if !r.status().is_success() {
            bail!("host returned {} for status", r.status());
        }

        let body: HostProjectStatus = r.json().await.context("host: malformed status body")?;
        if let Some(err) = &body.last_error {
            tracing::warn!(%project_id, host_error = %err, "host reports project error");
        }
        Ok(match body.status.as_str() {
            "running" => ProjectStatus::Running,
            "starting" => ProjectStatus::Starting,
            "stopped" => ProjectStatus::Stopped,
            _ => ProjectStatus::Error,
        })
    }
}

/// Turn a non-2xx host response into an error, without letting the host's body leak to our client.
async fn ensure_ok(r: reqwest::Response) -> Result<()> {
    let status = r.status();
    if status.is_success() {
        return Ok(());
    }
    // Read a bounded amount of the body for the *log*; it never reaches the client.
    let body = r.text().await.unwrap_or_default();
    let snippet: String = body.chars().take(500).collect();
    if status == reqwest::StatusCode::INSUFFICIENT_STORAGE {
        return Err(
            anyhow::Error::new(crate::orchestrator::HostRefusal::OutOfDisk)
                .context(format!("host returned {status}: {snippet}")),
        );
    }
    bail!("host returned {status}: {snippet}");
}
