//! Backend selection and state assembly.

use wheel_host::config::{Backend, Config};

fn cfg(backend: Backend, data_dir: &str) -> Config {
    Config {
        bind_addr: "127.0.0.1:0".into(),
        secret: "host-secret-at-least-16-chars".into(),
        backend,
        data_dir: data_dir.into(),
        engine_image: "wheel-engine:stub".into(),
        docker_network: "wheel".into(),
        engine_port: 7000,
        memory_bytes: 1 << 30,
        nano_cpus: 1_000_000_000,
        pids_limit: 512,
        start_timeout_secs: 30,
        engine_base_url: "http://127.0.0.1:7000".into(),
    }
}

#[test]
fn the_process_backend_refuses_to_start_rather_than_pretending() {
    // Starting on an unimplemented backend would report healthy while every project stayed dead.
    let err = wheel_host::build_sandbox(&cfg(Backend::Process, "/tmp"))
        .err()
        .expect("process backend is not implemented yet");
    assert!(err.to_string().contains("not implemented"), "{err}");
}

#[test]
fn the_external_backend_points_at_its_configured_engine() {
    let sandbox = wheel_host::build_sandbox(&cfg(Backend::External, "/tmp")).expect("external");
    assert_eq!(
        sandbox.engine_base(&uuid::Uuid::new_v4()),
        "http://127.0.0.1:7000"
    );
}

#[tokio::test]
async fn build_state_opens_its_store_under_the_data_dir() {
    let dir = std::env::temp_dir().join(format!("wheel-host-boot-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();

    let state = wheel_host::build_state(cfg(Backend::External, dir.to_str().unwrap()))
        .expect("state assembles");

    assert!(
        dir.join("host.db").exists(),
        "the store should be created under data_dir"
    );

    // And it is a working store, not just a file.
    let id = uuid::Uuid::new_v4();
    state
        .store
        .upsert(&id, "engine-secret", "vault")
        .await
        .unwrap();
    assert!(state.store.get(&id).await.unwrap().is_some());
}

#[test]
fn an_unwritable_data_dir_fails_loudly() {
    let err = wheel_host::build_state(cfg(Backend::External, "/proc/nonexistent/nope"));
    assert!(
        err.is_err(),
        "an unopenable store must fail at boot, not at first write"
    );
}
