// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

//! Docker sandbox backend: one container per project.
//!
//! Every Docker object name derives from a `Uuid` supplied by the API from its own database.
//! No client-controlled string ever reaches an image reference, mount source, container name, or
//! command argument.

use super::{Sandbox, Secrets, Status};
use crate::config::Config;
use anyhow::{Context, Result};
use async_trait::async_trait;
use bollard::query_parameters as qp;
use bollard::Docker;
use std::collections::HashMap;
use std::time::Duration;
use uuid::Uuid;

/// Seconds Docker gives the container's own SIGTERM handler before SIGKILL.
///
/// Docker's own default is 10s. The §4b spawn contract gives the engine up to ~25s to shut down on
/// its own (drain, wait for turns in flight, signal its agents), so the unadorned default would
/// SIGKILL an engine mid-drain — the same mistake as the process backend's timeout, before it was
/// raised (review round 2, finding 2).
const ENGINE_STOP_GRACE_SECS: i32 = 30;

pub struct DockerSandbox {
    docker: Docker,
    cfg: Config,
    http: reqwest::Client,
}

impl DockerSandbox {
    pub fn connect(cfg: Config) -> Result<Self> {
        let docker = Docker::connect_with_local_defaults()
            .context("connecting to the docker daemon (is the socket mounted?)")?;
        Ok(Self::with_client(docker, cfg))
    }

    /// Build a backend around an existing docker client.
    ///
    /// The container we ask for is a security decision — every capability dropped but two, no
    /// published ports, hard resource caps — and asserting it needs a daemon that records the
    /// request. This is how the tests point the backend at one.
    pub fn with_client(docker: Docker, cfg: Config) -> Self {
        Self {
            docker,
            cfg,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(5))
                .build()
                .expect("build http client"),
        }
    }

    async fn inspect_state(&self, id: &Uuid) -> Result<Option<String>> {
        match self
            .docker
            .inspect_container(
                &self.cfg.container_name(id),
                None::<qp::InspectContainerOptions>,
            )
            .await
        {
            Ok(c) => Ok(c
                .state
                .and_then(|s| s.status)
                .map(|s| s.to_string().to_ascii_lowercase())),
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            }) => Ok(None),
            Err(e) => Err(e).context("inspecting container"),
        }
    }

    /// Poll the engine's `/healthz` until it answers or we give up.
    ///
    /// `start` is specified to block until the engine is actually serving, so that a caller who
    /// gets 200 can immediately proxy to it. Reporting "running" the moment the container process
    /// exists would just move the race into the next request.
    async fn await_healthy(&self, id: &Uuid) -> Result<()> {
        let url = format!("{}/healthz", self.cfg.engine_url(id));
        let deadline =
            tokio::time::Instant::now() + Duration::from_secs(self.cfg.start_timeout_secs);
        let mut delay = Duration::from_millis(100);

        loop {
            if let Ok(r) = self.http.get(&url).send().await {
                if r.status().is_success() {
                    return Ok(());
                }
            }
            if tokio::time::Instant::now() >= deadline {
                anyhow::bail!(
                    "engine did not become healthy within {}s",
                    self.cfg.start_timeout_secs
                );
            }
            tokio::time::sleep(delay).await;
            // Back off gently so a slow start does not mean hundreds of probes.
            delay = (delay * 2).min(Duration::from_secs(2));
        }
    }

    async fn create(&self, id: &Uuid, secrets: &Secrets) -> Result<()> {
        let volume = self.cfg.volume_name(id);
        self.docker
            .create_volume(bollard::models::VolumeCreateOptions {
                name: Some(volume.clone()),
                labels: Some(HashMap::from([("wheel.project".into(), id.to_string())])),
                ..Default::default()
            })
            .await
            .context("creating project volume")?;

        let host_config = bollard::models::HostConfig {
            // Least privilege. A tenant's agents run arbitrary code by design, so the container is
            // treated as hostile: hard resource caps so one project cannot starve the machine every
            // other tenant shares, and every capability dropped, NONE added back.
            //
            // F007 (per-node uid isolation, docs/proposals/script-execution-scope.md) is NOT YET
            // IMPLEMENTED: `child_command` (wheel-engine/src/supervisor/mod.rs) clears a child's
            // environment but never calls setuid/setgid or `pre_exec` anywhere in the engine, so
            // every child on a project still runs as the container's own uid. An earlier version of
            // this comment claimed the engine "drops each child to its own per-node uid" and granted
            // CAP_SETUID/CAP_SETGID on that basis — false, and the grant was accordingly unused
            // capability surface on a container that treats its own tenant as hostile. Tracked M2:
            // add the grant back IN THE SAME COMMIT that lands the setuid/setgid calls, not before.
            cap_drop: Some(vec!["ALL".into()]),
            // Compatible with the (currently empty) capability set above: no_new_privs blocks
            // privilege *gain* through execve (setuid bits, file capabilities) regardless of what,
            // if anything, is granted.
            security_opt: Some(vec!["no-new-privileges".into()]),
            memory: Some(self.cfg.memory_bytes),
            nano_cpus: Some(self.cfg.nano_cpus),
            pids_limit: Some(self.cfg.pids_limit),
            network_mode: Some(self.cfg.docker_network.clone()),
            binds: Some(vec![format!("{volume}:/data")]),
            restart_policy: Some(bollard::models::RestartPolicy {
                name: Some(bollard::models::RestartPolicyNameEnum::UNLESS_STOPPED),
                ..Default::default()
            }),
            // Deliberately no port bindings: the engine must be unreachable from the host network.
            // Everything goes API -> host -> engine.
            ..Default::default()
        };

        let config = bollard::models::ContainerCreateBody {
            image: Some(self.cfg.engine_image.clone()),
            env: Some(vec![
                format!("WHEEL_PROJECT_ID={id}"),
                format!("WHEEL_ENGINE_SECRET={}", secrets.engine_secret),
                format!("WHEEL_VAULT_KEY={}", secrets.vault_key),
                // wow-agent-brief task 4 / docs/proposals/wheeld-first-class-cloud-api-key-policy.md:
                // fail-secure per project, computed by wheel-host itself — never a value a project's
                // own owner can influence.
                format!("WHEEL_HARNESS_AUTH={}", self.cfg.harness_auth_for(id)),
                format!("WHEEL_LISTEN=tcp://0.0.0.0:{}", self.cfg.engine_port),
                "WHEEL_DATA_DIR=/data".to_string(),
                "WHEEL_LOG=json".to_string(),
                // Selects the engine entrypoint from the shared host image.
                "WHEEL_ROLE=engine".to_string(),
            ]),
            labels: Some(HashMap::from([("wheel.project".into(), id.to_string())])),
            host_config: Some(host_config),
            ..Default::default()
        };

        self.docker
            .create_container(
                Some(qp::CreateContainerOptions {
                    name: Some(self.cfg.container_name(id)),
                    ..Default::default()
                }),
                config,
            )
            .await
            .map_err(|e| explain_create_failure(e, &self.cfg.engine_image))?;
        Ok(())
    }
}

/// Docker answers "no such image" with a bare 404, which surfaces to the user as a 500 on project
/// start and says nothing about what to do. Name the missing tag and the command that builds it.
fn explain_create_failure(e: bollard::errors::Error, image: &str) -> anyhow::Error {
    if let bollard::errors::Error::DockerResponseServerError {
        status_code: 404, ..
    } = e
    {
        return anyhow::anyhow!(
            "engine image {image} is not present on this docker daemon — build it with \
             `make engine-image`, or set ENGINE_IMAGE to an image that exists"
        );
    }
    anyhow::Error::new(e).context("creating project container")
}

#[async_trait]
impl Sandbox for DockerSandbox {
    async fn provision(&self, id: &Uuid, secrets: &Secrets) -> Result<()> {
        if self.inspect_state(id).await?.is_some() {
            return Ok(());
        }
        self.create(id, secrets).await
    }

    async fn start(&self, id: &Uuid, secrets: &Secrets) -> Result<()> {
        // Provision on demand: the API may PUT and start in quick succession, and a start for a
        // project whose container was reaped should heal rather than fail.
        self.provision(id, secrets).await?;

        match self
            .docker
            .start_container(
                &self.cfg.container_name(id),
                None::<qp::StartContainerOptions>,
            )
            .await
        {
            Ok(()) => {}
            // 304 = already started. Idempotent: a user click and a reconcile can race.
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 304, ..
            }) => {}
            Err(e) => return Err(e).context("starting container"),
        }
        self.await_healthy(id).await
    }

    async fn stop(&self, id: &Uuid) -> Result<()> {
        match self
            .docker
            .stop_container(
                &self.cfg.container_name(id),
                Some(qp::StopContainerOptions {
                    signal: None,
                    t: Some(ENGINE_STOP_GRACE_SECS),
                }),
            )
            .await
        {
            Ok(()) => Ok(()),
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 304, ..
            }) => Ok(()),
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            }) => Ok(()),
            Err(e) => Err(e).context("stopping container"),
        }
    }

    async fn restart(&self, id: &Uuid, secrets: &Secrets) -> Result<()> {
        self.stop(id).await?;
        self.start(id, secrets).await
    }

    async fn destroy(&self, id: &Uuid) -> Result<()> {
        if let Err(e) = self
            .docker
            .remove_container(
                &self.cfg.container_name(id),
                Some(qp::RemoveContainerOptions {
                    force: true,
                    v: false,
                    ..Default::default()
                }),
            )
            .await
        {
            match e {
                bollard::errors::Error::DockerResponseServerError {
                    status_code: 404, ..
                } => {}
                other => return Err(other).context("removing container"),
            }
        }
        if let Err(e) = self
            .docker
            .remove_volume(
                &self.cfg.volume_name(id),
                Some(qp::RemoveVolumeOptions { force: true }),
            )
            .await
        {
            match e {
                bollard::errors::Error::DockerResponseServerError {
                    status_code: 404, ..
                } => {}
                other => return Err(other).context("removing volume"),
            }
        }
        Ok(())
    }

    async fn status(&self, id: &Uuid) -> Result<Status> {
        Ok(match self.inspect_state(id).await?.as_deref() {
            None => Status::Stopped,
            Some("running") => Status::Running,
            Some("created") | Some("restarting") => Status::Starting,
            Some("paused") | Some("exited") | Some("removing") | Some("dead") => Status::Stopped,
            Some(_) => Status::Error,
        })
    }

    fn engine_base(&self, id: &Uuid) -> String {
        self.cfg.engine_url(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Review round 2, finding 2: Docker's own default stop timeout (10s) is shorter than an
    /// engine's own ~25s shutdown budget (2s HTTP drain + up to 20s draining turns + 3s SIGTERM
    /// grace for its agents), so leaving it at the default would SIGKILL the container mid-drain.
    #[test]
    #[allow(clippy::assertions_on_constants)]
    fn the_engine_stop_grace_covers_its_own_shutdown_budget() {
        assert!(
            ENGINE_STOP_GRACE_SECS >= 30,
            "ENGINE_STOP_GRACE_SECS is {ENGINE_STOP_GRACE_SECS}s, which is not enough room for a \
             ~25s engine shutdown to finish before being SIGKILLed"
        );
    }

    fn docker_404() -> bollard::errors::Error {
        bollard::errors::Error::DockerResponseServerError {
            status_code: 404,
            message: "No such image: wheel-engine:dev".into(),
        }
    }

    /// BUG-014: the compose stack defaulted to a tag nobody builds, and every project start came
    /// back as an opaque 500. The message has to name the tag and the command that produces it.
    #[test]
    fn a_missing_image_names_the_tag_and_how_to_build_it() {
        let msg = explain_create_failure(docker_404(), "wheel-engine:dev").to_string();
        assert!(msg.contains("wheel-engine:dev"), "{msg}");
        assert!(msg.contains("make engine-image"), "{msg}");
    }

    /// Every other docker failure keeps its own text: swallowing it into the image message would
    /// send an operator to rebuild an image over a permissions or daemon problem.
    #[test]
    fn other_docker_failures_are_not_reported_as_a_missing_image() {
        let e = bollard::errors::Error::DockerResponseServerError {
            status_code: 409,
            message: "Conflict. The container name is already in use".into(),
        };
        let msg = format!("{:#}", explain_create_failure(e, "wheel-engine:dev"));
        assert!(msg.contains("creating project container"), "{msg}");
        assert!(!msg.contains("make engine-image"), "{msg}");
    }
    /// Every docker object name is derived from a uuid the API generated, never from user input.
    #[test]
    fn object_names_are_derived_only_from_the_project_uuid() {
        let cfg = Config::for_tests("/tmp/wheel-docker-test");
        let id = Uuid::new_v4();
        assert!(cfg.container_name(&id).contains(&id.to_string()));
        assert!(cfg.volume_name(&id).contains(&id.to_string()));
        assert_ne!(cfg.container_name(&id), cfg.volume_name(&id));
    }
}
