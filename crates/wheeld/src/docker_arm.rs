//! The docker sandbox arm: one container per project, created through the filtering proxy.
//!
//! Chosen only by `WHEEL_SANDBOX=docker`. It never falls back: if anything here cannot be proved,
//! `wheeld` refuses to boot, because the alternative — serving with the isolation quietly gone — is
//! exactly the failure a per-project container exists to prevent.
//!
//! The docker socket is root on the machine, and this process also serves the internet-facing API.
//! So it is not given the socket. It is given `DOCKER_HOST`, which must name `wheel-docker-proxy`
//! (`wheel_host::docker_proxy`), and that is checked twice: by what the address says, and by asking
//! it a question the proxy refuses and a real daemon would answer.

use anyhow::{bail, Context, Result};
use bollard::Docker;
use std::collections::HashSet;
use std::sync::Arc;
use uuid::Uuid;
use wheel_host::config::{Backend, Config};
use wheel_host::sandbox::{docker::DockerSandbox, Sandbox};

/// Dev only: run against a daemon socket that is not filtered. Also requires `WHEEL_ENV=dev`,
/// because a flag that only warns is one `.env` copied from a laptop to a server away from on.
pub const ENV_ALLOW_RAW_SOCKET: &str = "WHEEL_ALLOW_RAW_DOCKER_SOCKET";

/// Where bollard connects when `DOCKER_HOST` is unset.
const DEFAULT_SOCKET: &str = "/var/run/docker.sock";

/// The unix socket `DOCKER_HOST` names, resolved as bollard resolves it (unset means the daemon's
/// default). Anything that is not a unix socket cannot be identity-checked, so it is refused.
pub fn docker_socket(docker_host: Option<&str>) -> Result<std::path::PathBuf, String> {
    match docker_host.filter(|h| !h.is_empty()) {
        None => Ok(DEFAULT_SOCKET.into()),
        Some(host) => host
            .strip_prefix("unix://")
            .map(std::path::PathBuf::from)
            .ok_or_else(|| {
                format!(
                    "DOCKER_HOST={host} is not a unix socket, so it cannot be the filtering proxy"
                )
            }),
    }
}

/// One `GET` over a unix socket; returns (status, body).
async fn get_over_socket(path: &std::path::Path, target: &str) -> std::io::Result<(u16, String)> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut stream = tokio::net::UnixStream::connect(path).await?;
    stream
        .write_all(
            format!("GET {target} HTTP/1.1\r\nhost: docker\r\nconnection: close\r\n\r\n")
                .as_bytes(),
        )
        .await?;
    let mut raw = Vec::new();
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        stream.read_to_end(&mut raw),
    )
    .await
    .map_err(|_| std::io::Error::other("the docker socket did not answer"))??;
    let text = String::from_utf8_lossy(&raw);
    let status = text
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    let body = text.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
    Ok((status, body))
}

/// Prove, by behaviour, that this socket is the filtering proxy: it says so, AND it refuses a call
/// a real daemon answers. Naming the socket proves nothing — an unset `DOCKER_HOST`, a symlink, or a
/// bind-mount at another path all reach a real daemon under a name that looks innocent.
pub async fn prove_filtered(socket: &std::path::Path, docker: &Docker) -> Result<(), String> {
    match get_over_socket(socket, wheel_host::docker_proxy::IDENTITY_PATH).await {
        Ok((200, body)) if body == wheel_host::docker_proxy::IDENTITY_BODY => {}
        Ok((status, _)) => {
            return Err(format!(
                "{} did not identify itself as the filtering proxy (answered {status})",
                socket.display()
            ))
        }
        Err(e) => return Err(format!("{} could not be reached: {e}", socket.display())),
    }
    match docker.version().await {
        Err(bollard::errors::Error::DockerResponseServerError {
            status_code: 403, ..
        }) => Ok(()),
        Ok(_) => {
            Err("the socket answered GET /version, so a real daemon is behind it unfiltered".into())
        }
        Err(e) => Err(format!("the docker socket misbehaved: {e}")),
    }
}

/// Everything docker mode needs, or a reason not to boot.
pub async fn connect(mut cfg: Config) -> Result<Arc<DockerSandbox>> {
    cfg.backend = Backend::Docker;

    if std::env::var(ENV_ALLOW_RAW_SOCKET).as_deref() == Ok("1") {
        if std::env::var("WHEEL_ENV").as_deref() != Ok("dev") {
            bail!(
                "{ENV_ALLOW_RAW_SOCKET}=1 is only honoured with WHEEL_ENV=dev; refusing to run the \
                 docker sandbox against a socket that is not proved to be filtered"
            );
        }
        tracing::warn!(
            "{ENV_ALLOW_RAW_SOCKET}=1: the docker socket this process uses is NOT proved to be the \
             filtering proxy. A bug in wheeld or the API is then root on this machine. Development \
             only."
        );
    } else {
        let host = std::env::var("DOCKER_HOST").ok();
        let socket = match docker_socket(host.as_deref()) {
            Ok(p) => p,
            Err(why) => bail!(
                "refusing to run the docker sandbox: {why}. Point DOCKER_HOST at wheel-docker-proxy's \
                 socket (unix:///path)"
            ),
        };
        let docker = Docker::connect_with_local_defaults().context("connecting to DOCKER_HOST")?;
        if let Err(why) = prove_filtered(&socket, &docker).await {
            bail!(
                "refusing to run the docker sandbox: {why}. DOCKER_HOST must be wheel-docker-proxy \
                 (for development against an unfiltered daemon: {ENV_ALLOW_RAW_SOCKET}=1 and WHEEL_ENV=dev)"
            );
        }
    }

    let sandbox = Arc::new(DockerSandbox::connect(cfg)?);
    // An inspection of a name no project has: the proxy admits it and the daemon answers "no such
    // container", which proves the whole path — proxy, daemon, credentials — in one allowed call.
    sandbox
        .status(&Uuid::nil())
        .await
        .context("the docker daemon is not reachable through DOCKER_HOST")?;
    Ok(sandbox)
}

/// The sandbox kind a data directory belongs to, recorded beside `host.db` and never silently
/// changed. Flipping the mode on a box with projects would provision EMPTY volumes for records whose
/// data sits untouched on disk, which looks exactly like data loss.
pub fn check_sandbox_kind(
    data_dir: &std::path::Path,
    mode: crate::config::SandboxMode,
) -> Result<()> {
    use crate::config::SandboxMode;
    let want = match mode {
        SandboxMode::Embedded => "embedded",
        SandboxMode::Docker => "docker",
    };
    let marker = data_dir.join("sandbox-kind");
    let existing = match std::fs::read_to_string(&marker) {
        Ok(k) => Some(k.trim().to_string()),
        // Written before the marker existed: embedded projects live under `projects/`.
        Err(_)
            if std::fs::read_dir(data_dir.join("projects"))
                .is_ok_and(|mut d| d.next().is_some()) =>
        {
            Some("embedded".to_string())
        }
        Err(_) => None,
    };
    match existing {
        Some(kind) if kind != want => bail!(
            "this data directory belongs to the {kind} sandbox, and WHEEL_SANDBOX selects {want}. \
             Switching would start every project from an empty volume while its real data sits \
             untouched. Keep WHEEL_SANDBOX as it was, or use a new data directory"
        ),
        Some(_) => Ok(()),
        None => std::fs::write(&marker, want).context("recording the sandbox kind"),
    }
}

/// `WHEEL_SANDBOX` and `SANDBOX_BACKEND` are two spellings of one decision and must not disagree.
pub fn check_backend_spelling(mode: crate::config::SandboxMode) -> Result<()> {
    use crate::config::SandboxMode;
    match (mode, std::env::var("SANDBOX_BACKEND").ok().as_deref()) {
        (SandboxMode::Docker, Some(b)) if b != "docker" => bail!(
            "WHEEL_SANDBOX=docker but SANDBOX_BACKEND={b}: two settings for one decision must agree"
        ),
        (SandboxMode::Embedded, Some("docker")) => bail!(
            "SANDBOX_BACKEND=docker but WHEEL_SANDBOX is not docker: set WHEEL_SANDBOX=docker, or unset SANDBOX_BACKEND"
        ),
        _ => Ok(()),
    }
}

/// After the normal reconcile: containers and volumes the store does not account for.
///
/// A container with no project row is STOPPED and reported at error level, never deleted: its
/// volume may be the only copy of someone's data (a store restored from an older backup looks
/// exactly like this), so removing it is an operator's decision. A container whose row says the
/// project should not be running is stopped, because docker's own `unless-stopped` restart would
/// otherwise bring it back before this process ever looked. A VOLUME with no project row AND no
/// container at all is a different case, treated differently (removed, not stopped) — see the
/// comment on that half below.
pub async fn reconcile_extras(
    sandbox: &DockerSandbox,
    store: &wheel_host::store::Store,
) -> Result<()> {
    let wanted: HashSet<Uuid> = store
        .all_desired_running()
        .await?
        .into_iter()
        .map(|r| r.id)
        .collect();
    let mut has_container: HashSet<Uuid> = HashSet::new();
    for (id, state) in sandbox.list_project_containers().await? {
        has_container.insert(id);
        let running = state == "running" || state == "restarting";
        if store.get(&id).await?.is_none() {
            tracing::error!(
                project = %id,
                %state,
                "a project container has no project record; stopping it and keeping its volume — \
                 remove it by hand once you are sure it is not someone's only copy"
            );
            if running {
                sandbox.stop(&id).await?;
            }
        } else if running && !wanted.contains(&id) {
            tracing::info!(project = %id, "stopping a container the store says should not be running");
            sandbox.stop(&id).await?;
        }
    }

    // A VOLUME with no container at all and no project record is a different case from the one
    // above, not the same rule applied to a different object: the container branch stops rather
    // than deletes because a container with real work in it might be the only copy of that work.
    // A volume with NO container was, by construction, never handed to an engine to write
    // anything into — the create sequence makes the volume before the container, so this is
    // exactly what an interrupted create (a crash, or the M3a project ceiling losing a race
    // between its count and the daemon's actual state) leaves behind. Reaped as the ceiling's
    // real backstop (adversary, #163: without this, 50 volume creates in a row are invisible to
    // both the ceiling and this reconcile forever). A volume whose project DOES have a container
    // is left to that container's own branch above, whatever the container's state.
    for id in sandbox.list_project_volumes().await? {
        if has_container.contains(&id) {
            continue;
        }
        if store.get(&id).await?.is_none() {
            tracing::error!(
                project = %id,
                "an orphaned project volume has no container and no project record; removing it \
                 (nothing was ever handed an engine to write into it)"
            );
            sandbox.remove_orphan_volume(&id).await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn docker_host_is_resolved_as_bollard_resolves_it_and_only_a_unix_socket_qualifies() {
        let default = std::path::PathBuf::from("/var/run/docker.sock");
        assert_eq!(docker_socket(None).unwrap(), default);
        assert_eq!(docker_socket(Some("")).unwrap(), default);
        assert_eq!(
            docker_socket(Some("unix:///run/wheel-docker/docker.sock")).unwrap(),
            std::path::PathBuf::from("/run/wheel-docker/docker.sock")
        );
        for bad in [
            "tcp://docker:2375",
            "http://127.0.0.1:2375",
            "ssh://root@box",
            "/run/x.sock",
        ] {
            assert!(docker_socket(Some(bad)).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_data_directory_keeps_its_sandbox_kind() {
        use crate::config::SandboxMode::{Docker, Embedded};
        let dir = std::path::PathBuf::from(format!("/tmp/wd-kind-{}", Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        check_sandbox_kind(&dir, Docker).unwrap();
        check_sandbox_kind(&dir, Docker).unwrap();
        let why = check_sandbox_kind(&dir, Embedded).unwrap_err().to_string();
        assert!(
            why.contains("docker") && why.contains("empty volume"),
            "{why}"
        );

        // A directory that has embedded projects but predates the marker is embedded, not blank.
        let legacy = std::path::PathBuf::from(format!("/tmp/wd-kind-{}", Uuid::new_v4().simple()));
        std::fs::create_dir_all(legacy.join("projects").join(Uuid::new_v4().to_string())).unwrap();
        assert!(check_sandbox_kind(&legacy, Docker).is_err());
        check_sandbox_kind(&legacy, Embedded).unwrap();
    }
}
