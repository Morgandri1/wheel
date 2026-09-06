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
use std::path::{Path, PathBuf};
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

/// The longest path a unix socket may be bound to: 104 bytes on macOS, 108 on Linux. The limit is
/// on the whole path rather than on any component, so a deep enough data directory exhausts it
/// before a project id and a file name are added.
#[cfg(target_os = "macos")]
const SUN_PATH_MAX: usize = 104;
#[cfg(not(target_os = "macos"))]
const SUN_PATH_MAX: usize = 108;

impl EmbeddedSandbox {
    /// Engines for the projects under `data_dir`, with their sockets somewhere they can be bound.
    pub fn for_data_dir(data_dir: PathBuf, start_timeout: Duration) -> Result<Self> {
        let run_dir = runtime_dir(&data_dir)?;
        Ok(Self::new(data_dir, run_dir, start_timeout))
    }

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
            // Empty, deliberately. This is the SSRF escape hatch for red-team probes, and a local
            // install has no reason to hold one open — the engine refuses it in production for the
            // same reason.
            tool_allow_hosts: Vec::new(),
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

/// Where the per-project engine sockets live.
///
/// Beside the data they belong to, so one `--data-dir` holds the whole install. But a socket path
/// that exceeds `SUN_PATH_MAX` cannot be bound at all — and a data directory under the system temp
/// dir can be long enough on its own — so past that point the sockets move to a short private
/// directory named for the install they serve. The alternative is `wheeld` failing to start a
/// project with "path must be shorter than SUN_LEN" from deep inside the engine.
fn runtime_dir(data_dir: &Path) -> Result<PathBuf> {
    let beside = data_dir.join("run");
    if socket_fits(&beside) {
        return Ok(beside);
    }

    let short = PathBuf::from("/tmp").join(format!(
        "wheel-{}-{}",
        unsafe { libc::geteuid() },
        digest8(data_dir)
    ));
    if !socket_fits(&short) {
        anyhow::bail!(
            "no directory short enough for an engine socket ({} is {} bytes, the limit is {})",
            short.display(),
            short.as_os_str().len(),
            SUN_PATH_MAX
        );
    }
    private_dir_of_ours(&short)?;
    tracing::info!(
        run_dir = %short.display(),
        "engine sockets are outside the data directory: its path is too long to bind one"
    );
    Ok(short)
}

fn socket_fits(run_dir: &Path) -> bool {
    let longest = run_dir.join(Uuid::nil().to_string()).join("engine.sock");
    longest.as_os_str().len() < SUN_PATH_MAX
}

/// Stable across restarts, so a second run of the same install finds the same directory.
fn digest8(path: &Path) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(path.as_os_str().as_encoded_bytes());
    format!("{:x}", h.finalize())[..8].to_string()
}

/// Create a 0700 directory, or accept an existing one only if it is ours and private.
///
/// This one is under `/tmp`, where any local account can create a name first. A directory someone
/// else owns would let them read every project's engine socket, so an unexpected owner or mode is
/// refused rather than corrected — correcting it would race whoever is holding it.
fn private_dir_of_ours(path: &Path) -> Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    // Created 0700 in one step rather than chmod-ed afterwards: between the two there is a window
    // in which the directory is world-readable, and a second wheeld starting at the same moment
    // would find it and — correctly — refuse to use it.
    match std::os::unix::fs::DirBuilderExt::mode(&mut std::fs::DirBuilder::new(), 0o700)
        .create(path)
    {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e).with_context(|| format!("creating {}", path.display())),
    }

    let meta = std::fs::metadata(path).with_context(|| format!("reading {}", path.display()))?;
    let mode = meta.permissions().mode() & 0o777;
    if !meta.is_dir() || meta.uid() != unsafe { libc::geteuid() } || mode & 0o077 != 0 {
        anyhow::bail!(
            "{} is not a private directory of ours (uid {}, mode {:o})",
            path.display(),
            meta.uid(),
            mode
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sockets_live_beside_a_data_dir_that_can_hold_them() {
        let dir = std::env::temp_dir().join(format!("wheeld-fit-{}", Uuid::new_v4()));
        // /tmp on Linux, /var/folders/... on macOS: either way this is what a user's data dir looks
        // like, and the answer must not depend on which one the test is running on.
        let expected = dir.join("run");
        let chosen = runtime_dir(&dir).unwrap();
        if socket_fits(&expected) {
            assert_eq!(chosen, expected);
        } else {
            assert!(
                socket_fits(&chosen),
                "fell back to a path that still cannot bind"
            );
        }
    }

    /// Unique per run: the short directory is named for the data directory, so two test processes
    /// sharing one would be testing each other rather than the code.
    fn too_deep() -> PathBuf {
        std::env::temp_dir().join(format!("{}{}", Uuid::new_v4(), "x".repeat(120)))
    }

    #[test]
    fn a_data_dir_too_deep_for_a_socket_gets_a_short_one() {
        let deep = too_deep();
        let chosen = runtime_dir(&deep).unwrap();
        assert!(!chosen.starts_with(&deep));
        assert!(
            socket_fits(&chosen),
            "{} still cannot bind a socket",
            chosen.display()
        );
        std::fs::remove_dir_all(chosen).ok();
    }

    #[test]
    fn the_short_directory_is_the_same_one_next_time() {
        let deep = too_deep();
        let chosen = runtime_dir(&deep).unwrap();
        assert_eq!(chosen, runtime_dir(&deep).unwrap());
        std::fs::remove_dir_all(chosen).ok();
    }

    #[test]
    fn the_short_directory_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let chosen = runtime_dir(&too_deep()).unwrap();
        let mode = std::fs::metadata(&chosen).unwrap().permissions().mode() & 0o777;
        std::fs::remove_dir_all(&chosen).ok();
        assert_eq!(
            mode,
            0o700,
            "{} is readable by other accounts",
            chosen.display()
        );
    }

    #[test]
    fn a_directory_someone_else_could_read_is_refused() {
        let dir = std::env::temp_dir().join(format!("wheeld-open-{}", Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        let refused = private_dir_of_ours(&dir);
        std::fs::remove_dir_all(&dir).ok();
        assert!(
            refused.is_err(),
            "a world-readable socket directory was accepted"
        );
    }
}
