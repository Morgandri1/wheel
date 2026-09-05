//! The docker sandbox backend, against a real docker daemon.
//!
//! These create and destroy actual containers. That is the point: the properties worth asserting
//! here — that no port is published, that every capability is dropped bar the two the engine needs,
//! that the child runs non-root — are properties of what docker was *told*, and a fake would only
//! prove that our own mock agrees with us.
//!
//! Skipped when there is no daemon, and hard-failed in CI, so this cannot quietly stop running.

use uuid::Uuid;
use wheel_host::config::{Backend, Config};
use wheel_host::sandbox::{Sandbox, Secrets};

const IMAGE: &str = "wheel-engine:dev";

fn cfg() -> Config {
    Config {
        bind_addr: "127.0.0.1:0".into(),
        secret: "host-secret-at-least-16-chars".into(),
        backend: Backend::Docker,
        data_dir: "/tmp".into(),
        engine_image: IMAGE.into(),
        docker_network: "wheel".into(),
        engine_port: 7000,
        memory_bytes: 512 * 1024 * 1024,
        nano_cpus: 500_000_000,
        pids_limit: 128,
        start_timeout_secs: 30,
        uid_range_start: 20_000,
        uid_stride: 64,
        run_dir: "/tmp/wheel-run-test".into(),
        engine_base_url: "http://127.0.0.1:7000".into(),
    }
}

fn secrets() -> Secrets {
    Secrets {
        engine_secret: "engine-secret-at-least-16-chars".into(),
        vault_key: "dmF1bHQta2V5LTMyLWJ5dGVzLWZvci10ZXN0cw==".into(),
    }
}

/// Is there a daemon, and is the engine image present?
async fn docker_ready() -> bool {
    let Ok(d) = bollard::Docker::connect_with_local_defaults() else {
        return false;
    };
    if d.version().await.is_err() {
        return false;
    }
    d.inspect_image(IMAGE).await.is_ok()
}

macro_rules! require_docker {
    () => {
        if !docker_ready().await {
            // Keyed on a promised daemon, not on CI. Asserting that every CI job has Docker and a
            // built image is the same mistake that turned main red over Postgres: the check has to
            // depend on the capability it needs, and the job that provides it says so.
            if std::env::var("WHEEL_CI_HAS_DOCKER").as_deref() == Ok("1") {
                panic!("WHEEL_CI_HAS_DOCKER=1 but no docker daemon or {IMAGE} is not built");
            }
            eprintln!("skipping: no docker daemon or {IMAGE} not built");
            return;
        }
    };
}

/// Always tear the container down, even when an assertion fails, so a failing run does not leave
/// containers behind for the next one to trip over.
async fn with_sandbox<F, Fut>(f: F)
where
    F: FnOnce(std::sync::Arc<dyn Sandbox>, Uuid) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let sandbox: std::sync::Arc<dyn Sandbox> = std::sync::Arc::new(
        wheel_host::sandbox::docker::DockerSandbox::connect(cfg()).expect("connect to docker"),
    );
    let id = Uuid::new_v4();
    let result = std::panic::AssertUnwindSafe(f(sandbox.clone(), id));
    let outcome = futures_util::FutureExt::catch_unwind(result).await;
    let _ = sandbox.destroy(&id).await;
    if let Err(p) = outcome {
        std::panic::resume_unwind(p);
    }
}

#[tokio::test]
async fn provision_is_idempotent_and_creates_the_container() {
    require_docker!();
    with_sandbox(|s, id| async move {
        s.provision(&id, &secrets()).await.expect("first provision");
        // Called again on every PUT from the API, so a second call must be a no-op, not a conflict.
        s.provision(&id, &secrets())
            .await
            .expect("second provision should be a no-op");

        let d = bollard::Docker::connect_with_local_defaults().unwrap();
        let c = d
            .inspect_container(
                &format!("wheel-p-{id}"),
                None::<bollard::query_parameters::InspectContainerOptions>,
            )
            .await
            .expect("container should exist after provision");
        let hc = c.host_config.expect("host config");

        assert_eq!(hc.cap_drop.as_deref(), Some(&["ALL".to_string()][..]));
        let cap_add = hc.cap_add.unwrap_or_default();
        assert!(cap_add.contains(&"SETUID".to_string()) && cap_add.contains(&"SETGID".to_string()));
        assert_eq!(
            cap_add.len(),
            2,
            "only SETUID and SETGID may be granted (F007)"
        );
        assert!(hc
            .security_opt
            .unwrap_or_default()
            .iter()
            .any(|o| o == "no-new-privileges"));
        assert_eq!(hc.memory, Some(512 * 1024 * 1024));
        assert_eq!(hc.pids_limit, Some(128));

        // The engine must be unreachable from the host network; everything goes API -> host -> engine.
        assert!(
            hc.port_bindings.unwrap_or_default().is_empty(),
            "the engine must not publish a port"
        );
    })
    .await;
}

// `start()` is deliberately not tested here.
//
// It blocks until the engine answers /healthz at `wheel-p-<id>:7000`, which is a docker-network
// address. A test process running natively on the host cannot resolve it — the container is
// healthy (docker's own HEALTHCHECK says so), we simply have no route to ask it. Asserting around
// that would mean either weakening the readiness gate or faking the probe, and both would test the
// fake rather than the thing.
//
// That path is covered where it can be exercised honestly: `infra/dev/e2e.py` drives
// create -> start -> proxied board through the compose stack, where the host runs inside the
// network alongside the engines it starts.

#[tokio::test]
async fn destroy_removes_container_and_volume_and_is_idempotent() {
    require_docker!();
    let sandbox = wheel_host::sandbox::docker::DockerSandbox::connect(cfg()).expect("connect");
    let id = Uuid::new_v4();
    sandbox.provision(&id, &secrets()).await.unwrap();

    sandbox.destroy(&id).await.expect("destroy");
    // Delete has to converge: a second call on an already-absent sandbox is success, not an error.
    sandbox
        .destroy(&id)
        .await
        .expect("destroy should be idempotent");

    let d = bollard::Docker::connect_with_local_defaults().unwrap();
    assert!(d
        .inspect_container(
            &format!("wheel-p-{id}"),
            None::<bollard::query_parameters::InspectContainerOptions>
        )
        .await
        .is_err());
    let vols = d
        .list_volumes(None::<bollard::query_parameters::ListVolumesOptions>)
        .await
        .unwrap();
    assert!(
        !vols
            .volumes
            .unwrap_or_default()
            .iter()
            .any(|v| v.name == format!("wheel-p-{id}-data")),
        "the project volume should be gone with the project"
    );
}

#[tokio::test]
async fn status_of_a_sandbox_that_was_never_created_is_stopped() {
    require_docker!();
    let sandbox = wheel_host::sandbox::docker::DockerSandbox::connect(cfg()).expect("connect");
    assert_eq!(
        sandbox.status(&Uuid::new_v4()).await.unwrap(),
        wheel_host::sandbox::Status::Stopped
    );
}

#[tokio::test]
async fn engine_base_addresses_the_container_by_name() {
    require_docker!();
    let sandbox = wheel_host::sandbox::docker::DockerSandbox::connect(cfg()).expect("connect");
    let id = Uuid::new_v4();
    let base = sandbox.engine_base(&id);
    assert!(base.contains(&format!("wheel-p-{id}")), "got {base}");
    assert!(base.ends_with(":7000"), "got {base}");
}
