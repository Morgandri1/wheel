//! Host configuration validation.
//!
//! One test, sequenced internally: environment variables are process-global, so parallel cases
//! would race each other rather than test anything.

use wheel_host::config::{Backend, Config};

fn base() {
    std::env::set_var("WHEEL_HOST_SECRET", "host-secret-at-least-16-chars");
    std::env::remove_var("SANDBOX_BACKEND");
    std::env::remove_var("WHEEL_ENV");
    std::env::remove_var("ENGINE_BASE_URL");
    std::env::remove_var("CONTAINER_MEMORY_MB");
    std::env::remove_var("BIND_ADDR");
    std::env::remove_var("PORT");
}

#[test]
fn host_config_validation() {
    // --- the secret ---------------------------------------------------------------------------
    // This bearer is the only thing between anything that can reach the port and control of every
    // tenant's sandbox, so a short one is refused rather than warned about.
    base();
    std::env::set_var("WHEEL_HOST_SECRET", "short");
    assert!(
        Config::from_env().is_err(),
        "a 5-char host secret must be refused"
    );

    base();
    std::env::remove_var("WHEEL_HOST_SECRET");
    assert!(
        Config::from_env().is_err(),
        "a missing host secret must be refused"
    );

    // --- backend selection --------------------------------------------------------------------
    base();
    let cfg = Config::from_env().expect("docker is the default backend");
    assert_eq!(cfg.backend, Backend::Docker);

    base();
    std::env::set_var("SANDBOX_BACKEND", "process");
    assert_eq!(Config::from_env().unwrap().backend, Backend::Process);

    base();
    std::env::set_var("SANDBOX_BACKEND", "nonsense");
    assert!(
        Config::from_env().is_err(),
        "an unrecognised backend must fail loudly rather than fall back to a default"
    );

    // The external backend performs no isolation at all — it just forwards to a URL — so it is
    // only allowed when dev has been asked for explicitly.
    base();
    std::env::set_var("SANDBOX_BACKEND", "external");
    assert!(
        Config::from_env().is_err(),
        "external must be refused outside dev: it is not a sandbox"
    );

    base();
    std::env::set_var("SANDBOX_BACKEND", "external");
    std::env::set_var("WHEEL_ENV", "dev");
    let cfg = Config::from_env().expect("external is allowed in dev");
    assert_eq!(cfg.backend, Backend::External);
    assert_eq!(
        cfg.engine_base_url, "http://127.0.0.1:7000",
        "default engine base"
    );

    base();
    std::env::set_var("SANDBOX_BACKEND", "external");
    std::env::set_var("WHEEL_ENV", "dev");
    std::env::set_var("ENGINE_BASE_URL", "http://engine.test:9999");
    assert_eq!(
        Config::from_env().unwrap().engine_base_url,
        "http://engine.test:9999"
    );

    // --- numeric parsing ----------------------------------------------------------------------
    base();
    std::env::set_var("CONTAINER_MEMORY_MB", "not-a-number");
    assert!(
        Config::from_env().is_err(),
        "a non-numeric limit must fail rather than silently default"
    );

    base();
    std::env::set_var("CONTAINER_MEMORY_MB", "2048");
    assert_eq!(Config::from_env().unwrap().memory_bytes, 2048 * 1024 * 1024);

    // --- derived names ------------------------------------------------------------------------
    base();
    let cfg = Config::from_env().unwrap();
    let id = uuid::Uuid::nil();
    assert_eq!(cfg.container_name(&id), format!("wheel-p-{id}"));
    assert_eq!(cfg.volume_name(&id), format!("wheel-p-{id}-data"));
    assert_eq!(cfg.engine_url(&id), format!("http://wheel-p-{id}:7000"));
    // --- where we listen ------------------------------------------------------------------------
    // The platform tells the app which port to use through $PORT, and probes that port. Binding
    // 7100 while the checker probes $PORT means every probe reaches nothing, the replica is
    // declared unhealthy and the container is stopped — which took this service down twice, once
    // per health-check path tried, before the cause turned out to be the port and not the path.
    base();
    assert_eq!(
        Config::from_env().unwrap().bind_addr,
        "0.0.0.0:7100",
        "the documented default, when the platform says nothing"
    );

    base();
    std::env::set_var("PORT", "8080");
    assert_eq!(Config::from_env().unwrap().bind_addr, "0.0.0.0:8080");

    base();
    std::env::set_var("PORT", " 7100 ");
    assert_eq!(
        Config::from_env().unwrap().bind_addr,
        "0.0.0.0:7100",
        "a padded PORT must not produce an unparseable address"
    );

    // An explicit BIND_ADDR still wins, for anyone binding a specific interface.
    base();
    std::env::set_var("PORT", "8080");
    std::env::set_var("BIND_ADDR", "127.0.0.1:9000");
    assert_eq!(Config::from_env().unwrap().bind_addr, "127.0.0.1:9000");

    base();
}
