//! The `process` sandbox backend: one unix uid per project, no container runtime.
//!
//! Railway gives us no docker daemon, so isolation here is the kernel's own: a dedicated uid per
//! project, a 0700 data directory owned by it, rlimits it cannot raise, and a pathname unix socket
//! for the control plane. Reviewed by ADVERSARY against F003/F007; the notes below record *why*
//! each step is the way it is, because every one of them is load-bearing and none of it is obvious
//! from the code alone.
//!
//! What this backend deliberately never does:
//!   * **No TCP.** On a shared kernel every loopback port is reachable by every other tenant, so a
//!     per-project port would undo the whole exercise. The engine listens on a unix socket only.
//!   * **No abstract sockets.** The abstract namespace ignores filesystem permissions entirely —
//!     any uid can connect. Only pathname sockets, inside a 0700 directory owned by the project.
//!   * **No secrets on argv.** `/proc/<pid>/cmdline` is world-readable, so the engine secret and
//!     vault key travel by environment (same-uid readable only) and never as arguments.

use super::{Sandbox, Secrets, Status};
use crate::config::Config;
use crate::store::Store;
use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use uuid::Uuid;

pub struct ProcessSandbox {
    cfg: Config,
    store: Arc<Store>,
    /// Live children, so stop/status act on the process we actually started rather than on a pid
    /// we read back from somewhere and hoped was still ours.
    children: Mutex<HashMap<Uuid, tokio::process::Child>>,
}

impl ProcessSandbox {
    pub fn new(cfg: Config, store: Arc<Store>) -> Self {
        Self {
            cfg,
            store,
            children: Mutex::new(HashMap::new()),
        }
    }

    pub fn project_dir(&self, id: &Uuid) -> PathBuf {
        PathBuf::from(&self.cfg.data_dir)
            .join("projects")
            .join(id.to_string())
    }

    /// Private temp directory, inside the project's own 0700 tree.
    ///
    /// A shared /tmp is a cross-tenant channel: predictable names, and on many systems readable
    /// metadata. Each project gets its own TMPDIR so nothing it writes is visible to a neighbour.
    pub fn tmp_dir(&self, id: &Uuid) -> PathBuf {
        self.project_dir(id).join("tmp")
    }

    pub fn run_dir(&self, id: &Uuid) -> PathBuf {
        PathBuf::from(&self.cfg.run_dir).join(id.to_string())
    }

    pub fn socket_path(&self, id: &Uuid) -> PathBuf {
        self.run_dir(id).join("engine.sock")
    }

    async fn uid_for(&self, id: &Uuid) -> Result<u32> {
        self.store
            .allocate_uid(id, self.cfg.uid_range_start, self.cfg.uid_stride)
            .await
    }
}

/// Create a directory owned by `uid` with the given mode.
///
/// The host does this *before* dropping privileges, and the engine never chowns anything: a child
/// that can change ownership can hand its files to another uid.
#[cfg(unix)]
fn make_owned_dir(path: &std::path::Path, uid: u32, gid: u32, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(path).with_context(|| format!("creating {}", path.display()))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
        .with_context(|| format!("setting mode on {}", path.display()))?;

    let c = std::ffi::CString::new(path.as_os_str().as_encoded_bytes())
        .context("path contained an interior nul")?;
    // SAFETY: `c` is a valid nul-terminated path for the duration of the call.
    let rc = unsafe { libc::chown(c.as_ptr(), uid, gid) };
    if rc != 0 {
        let err = std::io::Error::last_os_error();
        // Say what is actually wrong. Without root there is no way to give a directory to another
        // uid, so this backend cannot isolate anything — and quietly proceeding would leave every
        // project's data owned by the host user while looking like it had worked.
        if err.kind() == std::io::ErrorKind::PermissionDenied {
            bail!(
                "cannot chown {} to {uid}:{gid}: the process backend must run as root \
                 (it drops privileges per child); running unprivileged would leave every \
                 project's data owned by the host user",
                path.display()
            );
        }
        return Err(err).with_context(|| format!("chown {} to {uid}:{gid}", path.display()));
    }
    Ok(())
}

/// Everything the child does between fork and exec.
///
/// Order is the whole point and is not interchangeable:
///   1. `setgid` **before** `setuid`. After dropping the uid we no longer hold the privilege
///      needed to change the group, so the reverse order silently leaves the child in the host's
///      group.
///   2. `setgroups([])` before dropping too. Supplementary groups survive `setuid` and would carry
///      the host's group memberships into the tenant.
///   3. `no_new_privs` so no setuid binary or file capability can raise privilege again.
///   4. rlimits last, after the drop, so the tenant cannot raise what it did not set.
#[cfg(unix)]
fn drop_privileges(uid: u32, gid: u32, limits: &Rlimits) -> std::io::Result<()> {
    // SAFETY: called in the child between fork and exec. Only async-signal-safe syscalls here —
    // no allocation, no locks.
    unsafe {
        if libc::setgroups(0, std::ptr::null()) != 0 {
            return Err(std::io::Error::last_os_error());
        }
        if libc::setgid(gid) != 0 {
            return Err(std::io::Error::last_os_error());
        }
        if libc::setuid(uid) != 0 {
            return Err(std::io::Error::last_os_error());
        }
        // Belt and braces: if the uid did not actually change, exec'ing would run the engine as
        // the host. Refuse rather than continue.
        if libc::getuid() != uid || libc::geteuid() != uid {
            return Err(std::io::Error::other("uid did not drop"));
        }

        #[cfg(target_os = "linux")]
        {
            // PR_SET_NO_NEW_PRIVS = 38
            if libc::prctl(38, 1, 0, 0, 0) != 0 {
                return Err(std::io::Error::last_os_error());
            }
        }

        for (resource, value) in limits.as_pairs() {
            let lim = libc::rlimit {
                rlim_cur: value,
                rlim_max: value,
            };
            if libc::setrlimit(resource as _, &lim) != 0 {
                return Err(std::io::Error::last_os_error());
            }
        }
    }
    Ok(())
}

/// Per-child resource ceilings, tuned for sandboxes that compile code.
///
/// Agents developing Wheel run `cargo` and `pnpm` inside these sandboxes, and a build is the most
/// demanding thing that will ever run here. Limits sized for a web service quietly kill it, so the
/// defaults below deliberately favour "the build completes" over "the ceiling is tight", and every
/// one is overridable.
///
/// Two limits are off by default, for reasons specific to builds:
///
/// * **`RLIMIT_AS` (address space).** This is the classic build killer. It caps *virtual* address
///   space, not resident memory, and rustc reserves far more than it commits — thread stacks,
///   allocator arenas, mmapped crate metadata. A value that looks generous still fails as an
///   allocation error deep inside a dependency, which is close to undiagnosable. Real memory
///   containment belongs to the machine's cgroup, which counts what is actually used.
/// * **`RLIMIT_CPU`.** It is cumulative CPU seconds for the life of the process, not a rate. A
///   long-lived engine doing builds for days would eventually be SIGKILLed for no reason anyone
///   could connect to the cause.
#[derive(Clone, Copy)]
pub struct Rlimits {
    /// Processes/threads — the fork-bomb ceiling. Must still allow `cargo -j N` plus rustc's own
    /// threads.
    pub nproc: u64,
    /// Address space. `None` leaves it unlimited; see above.
    pub address_space: Option<u64>,
    /// Largest single file the tenant may create.
    pub fsize: u64,
    /// Open file descriptors. Builds open a lot of them.
    pub nofile: u64,
    /// Cumulative CPU seconds. `None` leaves it unlimited; see above.
    pub cpu: Option<u64>,
}

impl Rlimits {
    #[cfg(unix)]
    /// Returns `(resource, value)` pairs with the resource as `u32`.
    ///
    /// The type is not incidental: glibc declares `setrlimit(__rlimit_resource_t, ...)` where that
    /// is a `c_uint`, while macOS declares `setrlimit(c_int, ...)`. Typing these as `c_int` builds
    /// on a mac and fails to compile on Linux — which is exactly how this shipped a host image
    /// with no host binary in it.
    ///
    /// `as _` rather than a named type on purpose: naming either one is wrong on the other
    /// platform, and naming the *right* one is a no-op cast that clippy's `unnecessary_cast`
    /// rejects — which is how this failed the Linux gate after the type error was fixed.
    /// Inference picks correctly on both.
    fn as_pairs(&self) -> Vec<(u32, libc::rlim_t)> {
        let mut v = vec![
            (libc::RLIMIT_NPROC as _, self.nproc),
            (libc::RLIMIT_FSIZE as _, self.fsize),
            (libc::RLIMIT_NOFILE as _, self.nofile),
        ];
        if let Some(bytes) = self.address_space {
            v.push((libc::RLIMIT_AS as _, bytes));
        }
        if let Some(secs) = self.cpu {
            v.push((libc::RLIMIT_CPU as _, secs));
        }
        v
    }
}

impl From<&Config> for Rlimits {
    fn from(cfg: &Config) -> Self {
        Rlimits {
            nproc: cfg.rlimit_nproc,
            address_space: cfg.rlimit_address_space_bytes,
            fsize: cfg.rlimit_fsize_bytes,
            nofile: cfg.rlimit_nofile,
            cpu: cfg.rlimit_cpu_secs,
        }
    }
}

#[async_trait]
impl Sandbox for ProcessSandbox {
    /// Create the project's uid, directories and socket directory. Idempotent.
    async fn provision(&self, id: &Uuid, _secrets: &Secrets) -> Result<()> {
        // A unix socket path has to fit in `sockaddr_un.sun_path`, which is about 104 bytes. Past
        // that, `bind` fails at start with an error that says nothing about the real cause, so
        // check it here where we can name the offending path and the setting that produced it.
        let socket = self.socket_path(id);
        let len = socket.as_os_str().len();
        anyhow::ensure!(
            len < 100,
            "engine socket path is {len} bytes and must stay under 100 \
             (sockaddr_un limit): {} — shorten WHEEL_RUN_DIR",
            socket.display()
        );

        let uid = self.uid_for(id).await?;
        // One gid per project, numerically equal to its base uid: the shared workspaces in §3e are
        // group-writable within a project, and nothing outside it shares the group.
        let gid = uid;

        make_owned_dir(&self.project_dir(id), uid, gid, 0o700)?;
        make_owned_dir(&self.tmp_dir(id), uid, gid, 0o700)?;
        // 0700 so only this project's uids can reach the socket inside. Combined with a 0600
        // pathname socket, that is what makes unix-socket ownership sufficient isolation.
        make_owned_dir(&self.run_dir(id), uid, gid, 0o700)?;
        Ok(())
    }

    async fn start(&self, id: &Uuid, secrets: &Secrets) -> Result<()> {
        {
            // Idempotent: a second start while running returns the existing process rather than
            // spawning a rival supervisor for the same project.
            let children = self.children.lock().await;
            if children.contains_key(id) {
                return Ok(());
            }
        }

        let uid = self.uid_for(id).await?;
        let gid = uid;
        self.provision(id, secrets).await?;

        let socket = self.socket_path(id);
        // A stale socket from an unclean shutdown would make bind fail.
        let _ = std::fs::remove_file(&socket);

        let limits = Rlimits::from(&self.cfg);
        let mut cmd = tokio::process::Command::new("wheel-engine");
        cmd.env_clear()
            .env("WHEEL_ROLE", "engine")
            .env("WHEEL_PROJECT_ID", id.to_string())
            .env("WHEEL_ENGINE_SECRET", &secrets.engine_secret)
            .env("WHEEL_VAULT_KEY", &secrets.vault_key)
            .env("WHEEL_DATA_DIR", self.project_dir(id))
            .env("WHEEL_LISTEN", format!("unix://{}", socket.display()))
            .env("WHEEL_LOG", "json")
            .env("TMPDIR", self.tmp_dir(id))
            .env("HOME", self.project_dir(id))
            .env("PATH", "/usr/local/bin:/usr/bin:/bin")
            .kill_on_drop(false);

        #[cfg(unix)]
        {
            // `tokio::process::Command` exposes `pre_exec` directly on unix.
            // SAFETY: the closure runs in the child after fork and before exec, and calls only
            // async-signal-safe syscalls.
            unsafe {
                cmd.pre_exec(move || drop_privileges(uid, gid, &limits));
            }
        }

        // Record the uid we are dropping to. This is the line that proves per-project isolation is
        // actually happening in an environment where nobody can attach a shell — without it, the
        // only evidence is that nothing has gone wrong yet.
        tracing::info!(
            project = %id,
            uid,
            gid,
            socket = %socket.display(),
            "spawning engine under its own uid"
        );
        let child = cmd.spawn().context("spawning wheel-engine")?;
        self.children.lock().await.insert(*id, child);

        // Readiness is the engine answering on its socket, not merely a process existing: a child
        // that died on startup would otherwise be reported as running.
        let deadline = Instant::now() + Duration::from_secs(self.cfg.start_timeout_secs);
        while Instant::now() < deadline {
            if healthz_over_socket(&socket).await {
                return Ok(());
            }
            if let Some(child) = self.children.lock().await.get_mut(id) {
                if let Ok(Some(status)) = child.try_wait() {
                    bail!("engine exited during startup with {status}");
                }
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        self.stop(id).await.ok();
        bail!(
            "engine did not answer on its socket within {}s",
            self.cfg.start_timeout_secs
        )
    }

    async fn stop(&self, id: &Uuid) -> Result<()> {
        let Some(mut child) = self.children.lock().await.remove(id) else {
            return Ok(()); // already stopped; stop must converge
        };
        // SIGTERM first: the engine's contract is a clean shutdown within 15s (children stopped,
        // sqlite flushed). Killing outright would risk a torn database.
        #[cfg(unix)]
        if let Some(pid) = child.id() {
            // SAFETY: pid came from a child we spawned.
            unsafe {
                libc::kill(pid as libc::pid_t, libc::SIGTERM);
            }
        }

        let graceful = tokio::time::timeout(Duration::from_secs(15), child.wait()).await;
        if graceful.is_err() {
            tracing::warn!(project = %id, "engine ignored SIGTERM; killing");
            let _ = child.kill().await;
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
        // The uid allocation deliberately survives in the store: see `Store::allocate_uid`, a
        // recycled uid would inherit any file this project left behind.
        let _ = std::fs::remove_dir_all(self.project_dir(id));
        let _ = std::fs::remove_dir_all(self.run_dir(id));
        Ok(())
    }

    async fn status(&self, id: &Uuid) -> Result<Status> {
        let mut children = self.children.lock().await;
        let Some(child) = children.get_mut(id) else {
            return Ok(Status::Stopped);
        };
        match child.try_wait() {
            Ok(Some(_)) => {
                children.remove(id);
                Ok(Status::Stopped)
            }
            Ok(None) => Ok(Status::Running),
            Err(_) => Ok(Status::Error),
        }
    }

    fn engine_base(&self, id: &Uuid) -> String {
        // Carries the socket path rather than a host:port. The proxy dials the socket directly;
        // there is no TCP endpoint to address, by design.
        format!("unix://{}", self.socket_path(id).display())
    }
}

/// Probe `/healthz` over the engine's unix socket.
pub(crate) async fn healthz_over_socket(socket: &std::path::Path) -> bool {
    let Ok(stream) = tokio::net::UnixStream::connect(socket).await else {
        return false;
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut stream = stream;
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

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// Serve one HTTP response on a unix socket, then close.
    async fn socket_answering(path: std::path::PathBuf, response: &'static str) {
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf).await;
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.flush().await;
            }
        });
    }

    /// Short on purpose: a unix socket path must fit in `sun_path`, which is 104 bytes on macOS
    /// and 108 on Linux. The system temp directory plus a full uuid overruns it, and the failure
    /// is an opaque "path must be shorter than SUN_LEN" from bind rather than anything about the
    /// test. `/tmp` and eight hex characters leave plenty of room.
    fn sock(name: &str) -> std::path::PathBuf {
        let id = Uuid::new_v4().simple().to_string();
        std::path::PathBuf::from(format!(
            "/tmp/wh-{}-{}.sock",
            &name[..2.min(name.len())],
            &id[..8]
        ))
    }

    /// Readiness has to mean the engine answered, not merely that a process exists. Reporting a
    /// project `running` because a child was spawned would tell an operator their board is live
    /// while every request to it fails.
    #[tokio::test]
    async fn healthz_is_true_only_for_a_200() {
        let p = sock("ok");
        socket_answering(p.clone(), "HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n").await;
        assert!(healthz_over_socket(&p).await);
    }

    #[tokio::test]
    async fn a_non_200_is_not_ready() {
        // An engine that is up but failing its own health check is not ready, and treating any
        // reply as success would mask exactly the startup faults this probe exists to catch.
        for status in [
            "HTTP/1.1 500 Internal Server Error\r\ncontent-length: 0\r\n\r\n",
            "HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\n\r\n",
            "HTTP/1.1 503 Service Unavailable\r\ncontent-length: 0\r\n\r\n",
        ] {
            let p = sock("bad");
            socket_answering(p.clone(), status).await;
            assert!(!healthz_over_socket(&p).await, "accepted {status:?}");
        }
    }

    #[tokio::test]
    async fn a_socket_that_is_not_there_is_not_ready() {
        // The ordinary case while an engine is still starting, and the permanent case if it died.
        assert!(!healthz_over_socket(&sock("absent")).await);
    }

    #[tokio::test]
    async fn a_socket_that_says_nothing_is_not_ready() {
        // A listener that accepts and never replies. Without the read actually being checked this
        // would hang or, worse, count as ready.
        let p = sock("silent");
        let listener = tokio::net::UnixListener::bind(&p).unwrap();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                // Hold the connection open, say nothing, then drop it.
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                drop(stream);
            }
        });
        assert!(!healthz_over_socket(&p).await);
    }

    #[tokio::test]
    async fn garbage_on_the_socket_is_not_ready() {
        let p = sock("garbage");
        socket_answering(p.clone(), "this is not http at all").await;
        assert!(!healthz_over_socket(&p).await);
    }
}
