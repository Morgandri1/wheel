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
        uid_range_start: 20_000,
        uid_stride: 64,
        run_dir: "/tmp/wheel-run-test".into(),
        rlimit_nproc: 4096,
        rlimit_address_space_bytes: None,
        rlimit_fsize_bytes: 8 * 1024 * 1024 * 1024,
        rlimit_nofile: 16384,
        rlimit_cpu_secs: None,
        engine_base_url: "http://127.0.0.1:7000".into(),
    }
}

fn store_in(dir: &str) -> std::sync::Arc<wheel_host::store::Store> {
    std::fs::create_dir_all(dir).unwrap();
    std::sync::Arc::new(
        wheel_host::store::Store::open(&format!("{dir}/host.db")).expect("open store"),
    )
}

#[test]
fn the_process_backend_addresses_its_engine_by_unix_socket() {
    // Not a host:port. On a shared kernel every loopback port is reachable by every other tenant,
    // so a TCP endpoint here would undo the isolation the backend exists to provide.
    let dir = std::env::temp_dir().join(format!("wheel-boot-{}", uuid::Uuid::new_v4()));
    let dir = dir.to_str().unwrap();
    let sandbox =
        wheel_host::build_sandbox(&cfg(Backend::Process, dir), store_in(dir)).expect("process");
    let base = sandbox.engine_base(&uuid::Uuid::new_v4());
    assert!(base.starts_with("unix://"), "got {base}");
    assert!(base.ends_with("engine.sock"), "got {base}");
    assert!(
        !base.contains("127.0.0.1"),
        "a loopback address leaked into the engine base: {base}"
    );
}

#[test]
fn the_external_backend_points_at_its_configured_engine() {
    let dir = std::env::temp_dir().join(format!("wheel-boot-{}", uuid::Uuid::new_v4()));
    let dir = dir.to_str().unwrap();
    let sandbox =
        wheel_host::build_sandbox(&cfg(Backend::External, dir), store_in(dir)).expect("external");
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
