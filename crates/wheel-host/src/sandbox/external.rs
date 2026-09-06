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

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// Answer every request with one canned response, then close.
    async fn server(response: &'static str) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf).await;
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.flush().await;
            }
        });
        format!("http://{addr}")
    }

    /// A trailing slash in `ENGINE_BASE_URL` is the kind of thing an operator types once and then
    /// spends an hour on: it would make every proxied path `//v1/...`.
    #[test]
    fn a_trailing_slash_in_the_configured_base_is_dropped() {
        let sb = ExternalSandbox::new("http://127.0.0.1:7000/".into());
        assert_eq!(sb.engine_base(&Uuid::new_v4()), "http://127.0.0.1:7000");
    }

    #[test]
    fn every_project_shares_the_one_external_engine() {
        let sb = ExternalSandbox::new("http://127.0.0.1:7000".into());
        assert_eq!(
            sb.engine_base(&Uuid::new_v4()),
            sb.engine_base(&Uuid::new_v4())
        );
    }

    #[tokio::test]
    async fn lifecycle_calls_are_no_ops_because_the_engine_is_not_ours_to_manage() {
        let sb = ExternalSandbox::new("http://127.0.0.1:1".into());
        let id = Uuid::new_v4();
        let secrets = Secrets {
            engine_secret: "s".into(),
            vault_key: "k".into(),
        };
        assert!(sb.provision(&id, &secrets).await.is_ok());
        assert!(sb.start(&id, &secrets).await.is_ok());
        assert!(sb.restart(&id, &secrets).await.is_ok());
        assert!(sb.stop(&id).await.is_ok());
        assert!(sb.destroy(&id).await.is_ok());
    }

    #[tokio::test]
    async fn status_is_running_only_when_the_engine_answers_healthy() {
        let sb = ExternalSandbox::new(server("HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n").await);
        assert_eq!(sb.status(&Uuid::new_v4()).await.unwrap(), Status::Running);
    }

    #[tokio::test]
    async fn an_unhealthy_engine_reads_as_stopped() {
        let sb = ExternalSandbox::new(
            server("HTTP/1.1 503 Service Unavailable\r\ncontent-length: 0\r\n\r\n").await,
        );
        assert_eq!(sb.status(&Uuid::new_v4()).await.unwrap(), Status::Stopped);
    }

    /// Nothing listening must be a status, not an error: the host reports `stopped` and stays up.
    #[tokio::test]
    async fn an_engine_that_is_not_there_is_stopped_rather_than_an_error() {
        let sb = ExternalSandbox::new("http://127.0.0.1:1".into());
        assert_eq!(sb.status(&Uuid::new_v4()).await.unwrap(), Status::Stopped);
    }
}
