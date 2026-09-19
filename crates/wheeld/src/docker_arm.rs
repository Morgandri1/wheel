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
use std::collections::HashSet;
use bollard::Docker;
use std::sync::Arc;
use uuid::Uuid;
use wheel_host::config::{Backend, Config};
use wheel_host::sandbox::{docker::DockerSandbox, Sandbox};

/// Dev only: run against a daemon socket that is not filtered. Warned about on every boot.
pub const ENV_ALLOW_RAW_SOCKET: &str = "WHEEL_ALLOW_RAW_DOCKER_SOCKET";

const DAEMON_SOCKETS: [&str; 2] = ["/var/run/docker.sock", "/run/docker.sock"];

/// Whether `DOCKER_HOST` could name a filtering proxy at all. Only a unix socket that is not one of
/// the daemon's own can: a `tcp://` address may be anything, and it is not this function's job to
/// guess. Err carries the reason, worded for the operator.
pub fn could_be_the_proxy(docker_host: Option<&str>) -> Result<(), String> {
    let Some(host) = docker_host.map(str::trim).filter(|h| !h.is_empty()) else {
        return Err("DOCKER_HOST is not set, so the daemon's own socket would be used".into());
    };
    let Some(path) = host.strip_prefix("unix://") else {
        return Err(format!(
            "DOCKER_HOST={host} is not a unix socket, so it cannot be the filtering proxy"
        ));
    };
    if DAEMON_SOCKETS.contains(&path) {
        return Err(format!(
            "DOCKER_HOST={host} is the docker daemon's own socket"
        ));
    }
    Ok(())
}

/// Ask for something the proxy refuses (`GET /version`). A filtered socket answers 403; a raw
/// daemon answers, and a socket that cannot be reached at all is no better a place to run tenants.
pub async fn prove_filtered(docker: &Docker) -> Result<(), String> {
    match docker.version().await {
        Ok(_) => Err("the socket answered GET /version, so it is not the filtering proxy".into()),
        Err(bollard::errors::Error::DockerResponseServerError {
            status_code: 403, ..
        }) => Ok(()),
        Err(e) => Err(format!("the docker socket could not be reached: {e}")),
    }
}

/// Everything docker mode needs, or a reason not to boot.
pub async fn connect(mut cfg: Config) -> Result<Arc<DockerSandbox>> {
    cfg.backend = Backend::Docker;

    if std::env::var(ENV_ALLOW_RAW_SOCKET).as_deref() == Ok("1") {
        tracing::warn!(
            "{ENV_ALLOW_RAW_SOCKET}=1: the docker socket this process uses is NOT proved to be the \
             filtering proxy. A bug in wheeld or the API is then root on this machine. Development \
             only."
        );
    } else {
        let host = std::env::var("DOCKER_HOST").ok();
        if let Err(why) = could_be_the_proxy(host.as_deref()) {
            bail!(
                "refusing to run the docker sandbox: {why}. Point DOCKER_HOST at wheel-docker-proxy's \
                 socket (unix:///path). For development against an unfiltered daemon, set \
                 {ENV_ALLOW_RAW_SOCKET}=1"
            );
        }
        let docker = Docker::connect_with_local_defaults().context("connecting to DOCKER_HOST")?;
        if let Err(why) = prove_filtered(&docker).await {
            bail!(
                "refusing to run the docker sandbox: {why}. DOCKER_HOST must be wheel-docker-proxy \
                 (or set {ENV_ALLOW_RAW_SOCKET}=1 for development)"
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_unix_socket_that_is_not_the_daemons_own_could_be_the_proxy() {
        assert!(could_be_the_proxy(Some("unix:///run/wheel-docker/docker.sock")).is_ok());
        for bad in [
            None,
            Some(""),
            Some("  "),
            Some("unix:///var/run/docker.sock"),
            Some("unix:///run/docker.sock"),
            Some("tcp://docker:2375"),
            Some("http://127.0.0.1:2375"),
            Some("/run/wheel-docker/docker.sock"),
        ] {
            assert!(could_be_the_proxy(bad).is_err(), "{bad:?} was accepted");
        }
    }
}


/// After the normal reconcile: containers the store does not account for.
///
/// A container with no project row is STOPPED and reported at error level, never deleted: its
/// volume may be the only copy of someone's data (a store restored from an older backup looks
/// exactly like this), so removing it is an operator's decision. A container whose row says the
/// project should not be running is stopped, because docker's own `unless-stopped` restart would
/// otherwise bring it back before this process ever looked.
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
    for (id, state) in sandbox.list_project_containers().await? {
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
    Ok(())
}
