//! A `Sandbox` that runs each project's engine as a task in this process.
//!
//! The docker and process backends both hand a project its own OS process, which is what makes
//! per-tenant isolation possible. This one deliberately does not: `wheeld` is a single executable
//! for one person on one machine, where the tenants are all the same person, and the cost of that
//! choice is stated at boot rather than discovered later.
//!
//! Everything above the trait is unchanged — the host still proxies over a unix socket per project,
//! so the API's engine proxy and events bridge run exactly the code they run in production.

use anyhow::{Context, Result};
use async_trait::async_trait;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use uuid::Uuid;
use wheel_host::sandbox::{Sandbox, Secrets, Status};

pub struct EmbeddedSandbox {
    data_dir: PathBuf,
    run_dir: PathBuf,
    start_timeout: Duration,
    /// One task per project. Holding the handle is what makes stop possible: an engine we cannot
    /// stop is an engine that keeps serving after the project is deleted.
    engines: Mutex<HashMap<Uuid, tokio::task::JoinHandle<()>>>,
}

impl EmbeddedSandbox {
    pub fn new(data_dir: PathBuf, run_dir: PathBuf, start_timeout: Duration) -> Self {
        Self {
            data_dir,
            run_dir,
            start_timeout,
            engines: Mutex::new(HashMap::new()),
        }
    }

    pub fn project_dir(&self, id: &Uuid) -> PathBuf {
        self.data_dir.join("projects").join(id.to_string())
    }

    /// The socket is per project and lives in its own directory, matching the process backend, so
    /// the host proxies to it by exactly the same path.
    pub fn socket_path(&self, id: &Uuid) -> PathBuf {
        self.run_dir.join(id.to_string()).join("engine.sock")
    }

    async fn is_live(&self, id: &Uuid) -> bool {
        let engines = self.engines.lock().await;
        engines.get(id).is_some_and(|h| !h.is_finished())
    }
}

#[async_trait]
impl Sandbox for EmbeddedSandbox {
    async fn provision(&self, id: &Uuid, _secrets: &Secrets) -> Result<()> {
        // 0700 even here. The isolation this backend gives up is between a machine's projects, not
        // between its users: another account on the same laptop still has no business reading a
        // project's database.
        create_private_dir(&self.project_dir(id))?;
        create_private_dir(&self.socket_path(id).parent().unwrap().to_path_buf())?;
        Ok(())
    }

    async fn start(&self, id: &Uuid, secrets: &Secrets) -> Result<()> {
        // Idempotent, for the same reason the other backends are: a second start must return the
        // engine that is already running rather than race a second one onto the same database.
        if self.is_live(id).await {
            return Ok(());
        }

        self.provision(id, secrets).await?;
        let socket = self.socket_path(id);
        // A socket left by an unclean shutdown would make bind fail.
        let _ = std::fs::remove_file(&socket);

        let cfg = wheel_engine::Config {
            project_id: *id,
            engine_secret: secrets.engine_secret.clone(),
            vault_key: Some(secrets.vault_key.clone()),
            data_dir: self.project_dir(id),
            listen: wheel_core::ListenAddr::Unix(socket.clone()),
            json_logs: false,
        };

        let project = *id;
        let handle = tokio::spawn(async move {
            if let Err(e) = wheel_engine::serve(cfg).await {
                tracing::error!(project = %project, error = %format_args!("{e:#}"), "engine exited");
            }
        });
        self.engines.lock().await.insert(*id, handle);

        // Start means serving, not spawned: reporting success earlier just moves the race into the
        // caller's next request.
        let deadline = Instant::now() + self.start_timeout;
        while Instant::now() < deadline {
            if healthz(&socket).await {
                return Ok(());
            }
            if !self.is_live(id).await {
                anyhow::bail!("the engine exited before it became healthy");
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        self.stop(id).await.ok();
        anyhow::bail!(
            "engine did not become healthy within {}s",
            self.start_timeout.as_secs()
        )
    }

    async fn stop(&self, id: &Uuid) -> Result<()> {
        if let Some(handle) = self.engines.lock().await.remove(id) {
            handle.abort();
        }
        let _ = std::fs::remove_file(self.socket_path(id));
        Ok(())
    }

    async fn restart(&self, id: &Uuid, secrets: &Secrets) -> Result<()> {
        self.stop(id).await?;
        self.start(id, secrets).await
    }

    async fn destroy(&self, id: &Uuid) -> Result<()> {
        self.stop(id).await?;
        // Idempotent by contract: destroying a project that was never here is a success.
        match std::fs::remove_dir_all(self.project_dir(id)) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).context("removing the project directory"),
        }
        let _ = std::fs::remove_dir_all(self.socket_path(id).parent().unwrap());
        Ok(())
    }

    async fn status(&self, id: &Uuid) -> Result<Status> {
        if !self.is_live(id).await {
            return Ok(Status::Stopped);
        }
        // A task that is alive but not yet answering is starting, not running — the same
        // distinction the other backends draw, and the one the UI shows an operator.
        Ok(if healthz(&self.socket_path(id)).await {
            Status::Running
        } else {
            Status::Starting
        })
    }

    fn engine_base(&self, id: &Uuid) -> String {
        format!("unix://{}", self.socket_path(id).display())
    }
}

fn create_private_dir(path: &PathBuf) -> Result<()> {
    std::fs::create_dir_all(path).with_context(|| format!("creating {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .with_context(|| format!("securing {}", path.display()))?;
    }
    Ok(())
}

/// Is an engine answering on this socket?
///
/// A bare request rather than a client: the answer needed is "did something serve HTTP here", and
/// building a hyper client per poll to learn that would be more machinery than the question.
async fn healthz(socket: &std::path::Path) -> bool {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let Ok(mut stream) = tokio::net::UnixStream::connect(socket).await else {
        return false;
    };
    if stream
        .write_all(b"GET /healthz HTTP/1.1\r\nHost: engine\r\nConnection: close\r\n\r\n")
        .await
        .is_err()
    {
        return false;
    }
    let mut buf = [0u8; 64];
    match stream.read(&mut buf).await {
        Ok(n) if n > 0 => buf[..n].starts_with(b"HTTP/1.1 200"),
        _ => false,
    }
}
