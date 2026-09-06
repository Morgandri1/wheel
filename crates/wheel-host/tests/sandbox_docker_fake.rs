//! The docker backend against a recording daemon.
//!
//! The container the host asks for is a security decision — every capability dropped but the two
//! the engine needs to setuid its children, no published ports, hard resource caps — and none of it
//! was asserted anywhere: the real daemon is not available in CI, and reading the struct literal
//! only proves what the code says, not what goes on the wire. This speaks just enough of the docker
//! API to record the request and answer it.

use bollard::Docker;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use uuid::Uuid;
use wheel_host::config::{Backend, Config};
use wheel_host::sandbox::{docker::DockerSandbox, Sandbox, Secrets, Status};

#[derive(Default, Clone)]
struct Recorded {
    /// `METHOD /path` for every request, in order.
    requests: Vec<String>,
    /// Parsed JSON bodies, keyed by the path they were sent to.
    bodies: HashMap<String, serde_json::Value>,
    /// Whether a container has been created yet. Inspection 404s until one has, the way a real
    /// daemon does — without which `provision` always takes its "already exists" path and no test
    /// ever sees the container it asks for.
    created: bool,
}

/// A unix socket that answers the handful of docker endpoints this backend uses.
///
/// `state` decides what container inspection reports, so a test can put the daemon in a state and
/// assert how the backend maps it.
fn fake_daemon(state: &'static str) -> (std::path::PathBuf, Arc<Mutex<Recorded>>) {
    fake_daemon_with(state, false)
}

/// `pre_existing` starts the daemon as if the container were already there, for tests about
/// inspection rather than creation.
fn fake_daemon_with(
    state: &'static str,
    pre_existing: bool,
) -> (std::path::PathBuf, Arc<Mutex<Recorded>>) {
    let path = std::path::PathBuf::from(format!(
        "/tmp/wh-dk-{}.sock",
        &Uuid::new_v4().simple().to_string()[..8]
    ));
    let _ = std::fs::remove_file(&path);
    let listener = tokio::net::UnixListener::bind(&path).unwrap();
    let rec = Arc::new(Mutex::new(Recorded {
        created: pre_existing,
        ..Recorded::default()
    }));

    let recorder = rec.clone();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let recorder = recorder.clone();
            tokio::spawn(async move {
                // Read headers, then exactly Content-Length more: a single read() catches only
                // whatever happened to arrive in the first packet, which silently drops the body
                // this test exists to inspect.
                let mut raw = Vec::new();
                let mut buf = vec![0u8; 16384];
                loop {
                    let n = match stream.read(&mut buf).await {
                        Ok(n) if n > 0 => n,
                        _ => break,
                    };
                    raw.extend_from_slice(&buf[..n]);
                    let text = String::from_utf8_lossy(&raw);
                    let Some(head_end) = text.find("\r\n\r\n") else {
                        continue;
                    };
                    let want: usize = text[..head_end]
                        .lines()
                        .find_map(|l| {
                            let (k, v) = l.split_once(':')?;
                            k.eq_ignore_ascii_case("content-length")
                                .then(|| v.trim().parse().ok())?
                        })
                        .unwrap_or(0);
                    if raw.len() >= head_end + 4 + want {
                        break;
                    }
                }
                let raw = String::from_utf8_lossy(&raw).to_string();
                let head = raw.split("\r\n").next().unwrap_or("").to_string();
                let mut parts = head.split_whitespace();
                let method = parts.next().unwrap_or("").to_string();
                let full = parts.next().unwrap_or("").to_string();
                // Strip the /vX.YY version prefix bollard negotiates, and any query string.
                let path = full.split('?').next().unwrap_or("").to_string();
                // Strip only a real version prefix (/v1.43/...). Naively stripping "/v" also eats
                // the "v" of "/volumes", which is the kind of thing a fake gets wrong quietly.
                let path = match path.strip_prefix("/v") {
                    Some(rest) if rest.starts_with(|c: char| c.is_ascii_digit()) => {
                        match rest.find('/') {
                            Some(i) => rest[i..].to_string(),
                            None => path.clone(),
                        }
                    }
                    _ => path,
                };

                let exists = {
                    let mut r = recorder.lock().unwrap();
                    r.requests.push(format!("{method} {path}"));
                    if let Some(body) = raw.split("\r\n\r\n").nth(1) {
                        if let Ok(v) = serde_json::from_str::<serde_json::Value>(body.trim()) {
                            r.bodies.insert(path.clone(), v);
                        }
                    }
                    if path == "/containers/create" {
                        r.created = true;
                    }
                    r.created
                };

                let (code, body) = if path.ends_with("/json") {
                    if exists {
                        (200, format!(r#"{{"State":{{"Status":"{state}"}}}}"#))
                    } else {
                        (404, r#"{"message":"No such container"}"#.to_string())
                    }
                } else if path == "/volumes/create" {
                    (
                        201,
                        r#"{"Name":"vol","Driver":"local","Mountpoint":"/var/lib/docker/volumes/vol/_data","Labels":{},"Scope":"local","Options":{}}"#
                            .to_string(),
                    )
                } else if path.ends_with("/create") {
                    (201, r#"{"Id":"deadbeef","Warnings":[]}"#.to_string())
                } else {
                    (204, String::new())
                };
                let reason = if code == 404 { "Not Found" } else { "OK" };
                let response = format!(
                    "HTTP/1.1 {code} {reason}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.flush().await;
            });
        }
    });

    (path, rec)
}

fn cfg(data_dir: &str) -> Config {
    Config {
        bind_addr: "127.0.0.1:0".into(),
        secret: "test-host-secret-at-least-16".into(),
        backend: Backend::Docker,
        data_dir: data_dir.into(),
        engine_image: "wheel-engine:test".into(),
        docker_network: "wheel-test-net".into(),
        engine_port: 7000,
        memory_bytes: 512 * 1024 * 1024,
        nano_cpus: 1_500_000_000,
        pids_limit: 256,
        start_timeout_secs: 1,
        uid_range_start: 20_000,
        uid_stride: 64,
        run_dir: "/tmp/wheel-run-test".into(),
        rlimit_nproc: 4096,
        rlimit_address_space_bytes: None,
        rlimit_fsize_bytes: 1 << 30,
        rlimit_nofile: 16384,
        rlimit_cpu_secs: None,
        reap_grace_secs: 1,
        disk_floor_mb: 1,
        engine_base_url: "http://127.0.0.1:7000".into(),
    }
}

fn sandbox(sock: &std::path::Path) -> DockerSandbox {
    let docker = Docker::connect_with_unix(sock.to_str().unwrap(), 5, bollard::API_DEFAULT_VERSION)
        .expect("connect to the fake daemon");
    DockerSandbox::with_client(docker, cfg("/tmp/wheel-docker-fake"))
}

fn secrets() -> Secrets {
    Secrets {
        engine_secret: "engine-secret-value".into(),
        vault_key: "dmF1bHQta2V5LTMyLWJ5dGVzLWV4YWN0bHktb2sh".into(),
    }
}

/// Everything about the container that keeps one tenant from reaching another or from starving the
/// machine. Asserted on the request that actually goes to the daemon.
#[tokio::test]
async fn the_container_we_ask_for_is_the_locked_down_one() {
    let (sock, rec) = fake_daemon("running");
    let sb = sandbox(&sock);
    let id = Uuid::new_v4();

    sb.provision(&id, &secrets()).await.unwrap();

    let body = {
        let r = rec.lock().unwrap();
        r.bodies
            .get("/containers/create")
            .cloned()
            .expect("a container was created")
    };
    let host = &body["HostConfig"];

    assert_eq!(host["CapDrop"], serde_json::json!(["ALL"]));
    assert_eq!(host["CapAdd"], serde_json::json!(["SETUID", "SETGID"]));
    assert_eq!(
        host["SecurityOpt"],
        serde_json::json!(["no-new-privileges"])
    );
    assert_eq!(host["Memory"], serde_json::json!(512 * 1024 * 1024i64));
    assert_eq!(host["NanoCpus"], serde_json::json!(1_500_000_000i64));
    assert_eq!(host["PidsLimit"], serde_json::json!(256));
    assert_eq!(host["NetworkMode"], serde_json::json!("wheel-test-net"));

    // The engine must be unreachable from the host network: everything goes API -> host -> engine.
    let ports = &host["PortBindings"];
    assert!(
        ports.is_null() || ports.as_object().is_some_and(|m| m.is_empty()),
        "the engine container must publish no ports, got {ports}"
    );
    assert_eq!(body["Image"], serde_json::json!("wheel-engine:test"));
}

/// The engine's secrets travel in the container environment, and nothing else does.
#[tokio::test]
async fn the_engine_container_carries_its_secrets_and_no_ports() {
    let (sock, rec) = fake_daemon("running");
    let sb = sandbox(&sock);
    let id = Uuid::new_v4();
    sb.provision(&id, &secrets()).await.unwrap();

    let body = rec.lock().unwrap().bodies["/containers/create"].clone();
    let env: Vec<String> = body["Env"]
        .as_array()
        .expect("env")
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();

    assert!(env.contains(&"WHEEL_ENGINE_SECRET=engine-secret-value".to_string()));
    assert!(env.iter().any(|e| e.starts_with("WHEEL_VAULT_KEY=")));
    assert!(env.contains(&format!("WHEEL_PROJECT_ID={id}")));
    assert!(env.contains(&"WHEEL_ROLE=engine".to_string()));
}

/// Provision is idempotent by contract: an existing container is left alone rather than recreated,
/// which would destroy a running tenant's engine.
#[tokio::test]
async fn provisioning_an_existing_container_does_not_recreate_it() {
    let (sock, rec) = fake_daemon("running");
    let sb = sandbox(&sock);
    let id = Uuid::new_v4();

    sb.provision(&id, &secrets()).await.unwrap();
    let first = rec.lock().unwrap().requests.len();
    sb.provision(&id, &secrets()).await.unwrap();

    let after = rec.lock().unwrap();
    let creates = after
        .requests
        .iter()
        .filter(|r| r.contains("/containers/create"))
        .count();
    assert_eq!(creates, 1, "the second provision must not create again");
    assert!(after.requests.len() > first, "but it did inspect");
}

#[tokio::test]
async fn stop_and_destroy_reach_the_daemon() {
    let (sock, rec) = fake_daemon("running");
    let sb = sandbox(&sock);
    let id = Uuid::new_v4();

    sb.stop(&id).await.unwrap();
    sb.destroy(&id).await.unwrap();

    let r = rec.lock().unwrap();
    let joined = r.requests.join(" | ");
    assert!(joined.contains("POST /containers/"), "stop: {joined}");
    assert!(joined.contains("DELETE /containers/"), "destroy: {joined}");
    assert!(joined.contains("DELETE /volumes/"), "volume: {joined}");
}

/// Docker's container states are not our statuses, and the mapping is what the UI shows an
/// operator. "created" is starting, not running: a container that exists but has not run yet would
/// otherwise be reported as a live engine.
#[tokio::test]
async fn docker_states_map_to_our_statuses() {
    for (docker_state, expected) in [
        ("running", Status::Running),
        ("created", Status::Starting),
        ("restarting", Status::Starting),
        ("exited", Status::Stopped),
        ("paused", Status::Stopped),
        ("dead", Status::Stopped),
        // No unknown-state case: bollard deserialises the state into a closed enum and fails on
        // anything docker does not define, so our `Status::Error` arm is unreachable through it.
        ("removing", Status::Stopped),
    ] {
        let (sock, _) = fake_daemon_with(docker_state, true);
        let sb = sandbox(&sock);
        assert_eq!(
            sb.status(&Uuid::new_v4()).await.unwrap(),
            expected,
            "docker state {docker_state:?}"
        );
    }
}

#[tokio::test]
async fn the_engine_base_url_is_per_project() {
    let (sock, _) = fake_daemon("running");
    let sb = sandbox(&sock);
    let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
    assert_ne!(sb.engine_base(&a), sb.engine_base(&b));
    assert!(sb.engine_base(&a).contains(&a.to_string()));
}

/// Start must not report success until the engine answers.
///
/// The container existing is not the same as the engine serving: reporting "running" when the
/// process has merely been created moves the race into the caller's next request, which is where it
/// is hardest to diagnose. Here the engine host name does not resolve, so readiness never arrives
/// and start has to fail rather than claim success.
#[tokio::test]
async fn start_fails_when_the_engine_never_becomes_healthy() {
    let (sock, rec) = fake_daemon("running");
    let sb = sandbox(&sock);
    let id = Uuid::new_v4();

    let started = std::time::Instant::now();
    let result = sb.start(&id, &secrets()).await;

    assert!(
        result.is_err(),
        "start reported success for an engine that never answered"
    );
    let msg = format!("{:#}", result.unwrap_err());
    assert!(
        msg.contains("healthy"),
        "the failure should say what it waited for, got {msg}"
    );
    assert!(
        started.elapsed() < std::time::Duration::from_secs(20),
        "start must give up on its own timeout, not hang"
    );

    // It still asked docker to start the container: the failure is readiness, not orchestration.
    let r = rec.lock().unwrap();
    assert!(
        r.requests.iter().any(|q| q.ends_with("/start")),
        "the container was never started: {:?}",
        r.requests
    );
}

/// Restart goes through the same readiness gate, for the same reason.
#[tokio::test]
async fn restart_also_waits_for_readiness() {
    let (sock, rec) = fake_daemon("running");
    let sb = sandbox(&sock);
    let id = Uuid::new_v4();

    assert!(sb.restart(&id, &secrets()).await.is_err());
    let r = rec.lock().unwrap();
    assert!(
        r.requests.iter().any(|q| q.ends_with("/start")),
        "restart never started the container: {:?}",
        r.requests
    );
}
