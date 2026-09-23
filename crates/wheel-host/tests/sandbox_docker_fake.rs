// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

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
use wheel_host::docker_proxy::policy::RunRoot;
use wheel_host::sandbox::{docker::DockerSandbox, Sandbox, Secrets, Status};

/// The run-root channel's directory ownership is the whole mechanism (§3, entrypoint.sh's fixed
/// `AGENT_UID`/`AGENT_GID`), and only root can chown a directory to another uid. Skipped off-root
/// so a laptop run stays useful; hard-failed where root was promised — the same pattern
/// `sandbox_process.rs` already uses for the identical reason on the process backend.
macro_rules! require_root {
    () => {
        #[cfg(unix)]
        {
            let is_root = unsafe { libc::geteuid() } == 0;
            if !is_root {
                if std::env::var("WHEEL_CI_HAS_ROOT").as_deref() == Ok("1") {
                    panic!("WHEEL_CI_HAS_ROOT=1 but this process is not root");
                }
                eprintln!(
                    "skipping: the docker run-root channel needs root to chown the socket dir"
                );
                return;
            }
        }
    };
}

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
    fake_daemon_with(state, false, None)
}

/// `pre_existing` starts the daemon as if the container were already there, for tests about
/// inspection rather than creation. `health`, when set, is docker's own HEALTHCHECK status word
/// (`starting`/`healthy`/`unhealthy`) reported alongside `state` — `None` matches a container
/// with no HEALTHCHECK result yet reported (what every existing caller of this fixture predates).
fn fake_daemon_with(
    state: &'static str,
    pre_existing: bool,
    health: Option<&'static str>,
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
                // Strip only a real version prefix (/v1.43/...). Naively stripping "/v" also eats
                // the "v" of "/volumes", which is the kind of thing a fake gets wrong quietly.
                let strip_version = |s: &str| match s.strip_prefix("/v") {
                    Some(rest) if rest.starts_with(|c: char| c.is_ascii_digit()) => {
                        match rest.find('/') {
                            Some(i) => rest[i..].to_string(),
                            None => s.to_string(),
                        }
                    }
                    _ => s.to_string(),
                };
                // `path` (no query string) keys state lookups and recorded bodies; `full` (query
                // string kept) is what `requests` records, so a test can see e.g. a stop timeout
                // that only ever travels as `?t=30`.
                let path = strip_version(full.split('?').next().unwrap_or(""));
                let full = strip_version(&full);

                // What a real daemon remembers of the create: its labels, echoed back on inspection
                // (the backend reads its `wheel.spec` label to tell a current container from a stale one).
                let mut labels_json = String::new();
                let exists = {
                    let mut r = recorder.lock().unwrap();
                    r.requests.push(format!("{method} {full}"));
                    if let Some(body) = raw.split("\r\n\r\n").nth(1) {
                        if let Ok(v) = serde_json::from_str::<serde_json::Value>(body.trim()) {
                            r.bodies.insert(path.clone(), v);
                        }
                    }
                    if path == "/containers/create" {
                        r.created = true;
                    }
                    if let Some(l) = r
                        .bodies
                        .get("/containers/create")
                        .map(|b| b["Labels"].clone())
                    {
                        if !l.is_null() {
                            labels_json = format!(r#","Config":{{"Labels":{l}}}"#);
                        }
                    }
                    r.created
                };

                let (code, body) = if path.ends_with("/json") {
                    if exists {
                        let health_json = match health {
                            Some(h) => format!(r#","Health":{{"Status":"{h}"}}"#),
                            None => String::new(),
                        };
                        (
                            200,
                            format!(
                                r#"{{"State":{{"Status":"{state}"{health_json}}}{labels_json}}}"#
                            ),
                        )
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
        reconcile_concurrency: 8,
        engine_base_url: "http://127.0.0.1:7000".into(),
        docker_run_root: None,
        oauth_allowed_projects: Vec::new(),
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
    // No capability is added back. F007 (per-node uid isolation) is not yet implemented -- nothing
    // in the engine calls setuid/setgid -- so granting SETUID/SETGID here would be capability
    // surface on a hostile container with no code that uses it. Add it back in the same commit that
    // lands the setuid/setgid calls.
    assert_eq!(host["CapAdd"], serde_json::Value::Null);
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

/// `WHEEL_HARNESS_AUTH_OAUTH_PROJECTS` (wow-agent-brief task 4): the container the docker backend
/// actually creates carries the allowlist-derived value, not a fixed constant. Two separate fake
/// daemons: the fake's "already exists" state is a single flag flipped by the first `create`, not
/// keyed per container id, so reusing one daemon for a second project's provision would see it as
/// already-existing and skip `create` entirely (`provisioning_an_existing_container_does_not_recreate_it`
/// pins exactly that behaviour) — a real daemon would of course track the two containers separately.
#[tokio::test]
async fn the_engine_container_carries_the_allowlist_derived_harness_auth_value() {
    let allowed = Uuid::new_v4();
    let other = Uuid::new_v4();

    let (sock, rec) = fake_daemon("running");
    let docker = Docker::connect_with_unix(sock.to_str().unwrap(), 5, bollard::API_DEFAULT_VERSION)
        .expect("connect to the fake daemon");
    let mut config = cfg("/tmp/wheel-docker-fake-harness-auth-allowed");
    config.oauth_allowed_projects = vec![allowed];
    let sb = DockerSandbox::with_client(docker, config);
    sb.provision(&allowed, &secrets()).await.unwrap();
    let body = rec.lock().unwrap().bodies["/containers/create"].clone();
    let env: Vec<String> = body["Env"]
        .as_array()
        .expect("env")
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert!(
        env.contains(&"WHEEL_HARNESS_AUTH=oauth-token".to_string()),
        "an allowlisted project must get oauth-token: {env:?}"
    );

    let (sock, rec) = fake_daemon("running");
    let docker = Docker::connect_with_unix(sock.to_str().unwrap(), 5, bollard::API_DEFAULT_VERSION)
        .expect("connect to the fake daemon");
    let mut config = cfg("/tmp/wheel-docker-fake-harness-auth-other");
    config.oauth_allowed_projects = vec![allowed];
    let sb = DockerSandbox::with_client(docker, config);
    sb.provision(&other, &secrets()).await.unwrap();
    let body = rec.lock().unwrap().bodies["/containers/create"].clone();
    let env: Vec<String> = body["Env"]
        .as_array()
        .expect("env")
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert!(
        env.contains(&"WHEEL_HARNESS_AUTH=api-key-only".to_string()),
        "a project not on the allowlist must get the fail-secure default: {env:?}"
    );
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

/// Review round 2, finding 2: docker's own default stop timeout (10s) is shorter than the engine's
/// own ~25s shutdown budget, so `stop()` must ask for a longer one. A prior version of this
/// coverage lived only as a unit test asserting `ENGINE_STOP_GRACE_SECS >= 30` — a bare constant
/// that stayed true even after a reviewer reverted the call site in `stop()` back to a shorter,
/// hardcoded number. This asserts the value that actually reaches the daemon on the wire.
#[tokio::test]
async fn stop_asks_the_daemon_for_the_full_engine_shutdown_grace() {
    let (sock, rec) = fake_daemon("running");
    let sb = sandbox(&sock);
    let id = Uuid::new_v4();

    sb.stop(&id).await.unwrap();

    let r = rec.lock().unwrap();
    let stop_request = r
        .requests
        .iter()
        .find(|req| req.contains("/stop"))
        .unwrap_or_else(|| panic!("no stop request seen: {:?}", r.requests));
    assert!(
        stop_request.contains("t=30"),
        "stop must ask docker to wait out the engine's full ~25s shutdown budget before \
         killing it, got {stop_request:?}"
    );
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
        let (sock, _) = fake_daemon_with(docker_state, true, None);
        let sb = sandbox(&sock);
        assert_eq!(
            sb.status(&Uuid::new_v4()).await.unwrap(),
            expected,
            "docker state {docker_state:?}"
        );
    }
}

/// A container docker reports as `running` whose own HEALTHCHECK hasn't completed its first cycle
/// yet (`health: "starting"`, always true for the whole `--start-period`, 10s on this image) must
/// still read as `Running`, not `Starting` — that health word is not a negative signal, and
/// `start()` has already confirmed the engine live via a direct, more current `/healthz` probe of
/// its own before this status is ever asked for. Getting this wrong meant a project read back
/// `starting` for up to ten seconds after a start that had already succeeded.
///
/// `unhealthy`, in contrast, IS a negative signal (docker ran the check and it failed) and must
/// still demote — pinned alongside so the fix doesn't overcorrect into ignoring health entirely.
#[tokio::test]
async fn a_running_container_still_awaiting_its_first_healthcheck_reads_as_running() {
    for health in [Some("starting"), Some("healthy"), None] {
        let (sock, _) = fake_daemon_with("running", true, health);
        let sb = sandbox(&sock);
        assert_eq!(
            sb.status(&Uuid::new_v4()).await.unwrap(),
            Status::Running,
            "health={health:?}"
        );
    }
}

#[tokio::test]
async fn a_running_container_that_actually_failed_its_healthcheck_reads_as_error() {
    let (sock, _) = fake_daemon_with("running", true, Some("unhealthy"));
    let sb = sandbox(&sock);
    let err = sb.status(&Uuid::new_v4()).await.unwrap_err();
    assert!(format!("{err:#}").contains("unhealthy"), "{err:#}");
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

// --- M3b: the run-root unix-socket channel -----------------------------------------------------
//
// Without `docker_run_root`, none of this changes at all (every test above still runs with it
// unset). With it set, three things have to move together or the proxy's own `policy.rs` refuses
// the create outright: `WHEEL_LISTEN` in the env, the extra `Binds` entry, and `engine_base()` —
// this is what proves they actually agree, not just that each one individually looks right.

fn run_root_scratch() -> (RunRoot, String) {
    // Short on purpose: `<this>/<project-uuid>/engine.sock` has to clear the sockaddr_un length
    // guard `create()` itself enforces (~100 bytes), the same constraint `sandbox_process.rs`'s
    // `run_dir` fixtures respect for the identical reason.
    let dir = std::env::temp_dir().join(format!(
        "wh-rr-{}",
        &Uuid::new_v4().simple().to_string()[..8]
    ));
    let path = dir.to_str().unwrap().to_string();
    (RunRoot::new(&path).unwrap(), path)
}

fn sandbox_with_run_root(sock: &std::path::Path, run_root: RunRoot) -> DockerSandbox {
    let docker = Docker::connect_with_unix(sock.to_str().unwrap(), 5, bollard::API_DEFAULT_VERSION)
        .expect("connect to the fake daemon");
    let mut config = cfg("/tmp/wheel-docker-fake-run-root");
    config.docker_run_root = Some(run_root);
    DockerSandbox::with_client(docker, config)
}

/// No run root configured: the engine base is the plain per-project TCP URL this backend has
/// always used, unchanged by M3b for every project that has not opted in.
#[tokio::test]
async fn without_a_run_root_the_engine_base_stays_tcp() {
    let (sock, _) = fake_daemon("running");
    let sb = sandbox(&sock);
    let id = Uuid::new_v4();
    let base = sb.engine_base(&id);
    assert!(base.starts_with("http://"), "got {base}");
}

/// With a run root, `engine_base()` is a host-visible unix socket path under it — never TCP, and
/// never the container-internal `/run/wheel` path (which this host, outside the container, could
/// not dial at all).
#[tokio::test]
async fn with_a_run_root_the_engine_base_is_a_host_visible_unix_socket() {
    let (sock, _) = fake_daemon("running");
    let (run_root, path) = run_root_scratch();
    let sb = sandbox_with_run_root(&sock, run_root);
    let id = Uuid::new_v4();

    let base = sb.engine_base(&id);
    assert!(base.starts_with("unix://"), "got {base}");
    assert!(base.ends_with("engine.sock"), "got {base}");
    assert!(
        base.contains(&path) && base.contains(&id.to_string()),
        "must be under the configured run root, per project: got {base}"
    );
    assert!(
        !base.contains("/run/wheel"),
        "must be the HOST path, not the container-internal mount point: got {base}"
    );
}

/// The create the daemon actually receives: `WHEEL_LISTEN` switches to the fixed unix path the
/// proxy's `policy::ENGINE_SOCKET` expects, and `Binds` grows the run-root entry alongside (never
/// instead of) the data volume bind — dropping the data bind would silently lose every engine's
/// sqlite file on the next recreate.
#[tokio::test]
async fn provision_with_a_run_root_binds_the_socket_dir_and_switches_listen_to_unix() {
    require_root!();
    let (sock, rec) = fake_daemon("running");
    let (run_root, path) = run_root_scratch();
    let sb = sandbox_with_run_root(&sock, run_root);
    let id = Uuid::new_v4();

    sb.provision(&id, &secrets()).await.unwrap();

    let body = rec.lock().unwrap().bodies["/containers/create"].clone();
    let env: Vec<String> = body["Env"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert!(
        env.contains(&"WHEEL_LISTEN=unix:///run/wheel/engine.sock".to_string()),
        "got {env:?}"
    );

    let binds: Vec<String> = body["HostConfig"]["Binds"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert!(
        binds.iter().any(|b| b.ends_with(":/data")),
        "the data volume bind must survive: {binds:?}"
    );
    assert!(
        binds
            .iter()
            .any(|b| b == &format!("{path}/{id}:/run/wheel")),
        "must bind exactly the same path the proxy's RunRoot::bind_for would produce: {binds:?}"
    );

    // The host-side directory this bind names has to actually exist before the container starts,
    // or docker creates it as root:root and the containerized engine (uid 10001) gets EACCES.
    let meta = std::fs::metadata(format!("{path}/{id}")).expect("run-root dir was created");
    assert!(meta.is_dir());
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        assert_eq!(meta.uid(), 10001, "must be owned by AGENT_UID");
        assert_eq!(meta.gid(), 10001, "must be owned by AGENT_GID");
    }
}

/// ADVERSARY review of #165: the sockaddr_un length guard in `create()` had no regression pin at
/// any privilege level — every other test that reaches `create()` is `require_root!()`-gated, so
/// a reverted `ensure!` would have shipped silently. This one needs no root: the guard fires and
/// returns before `make_owned_dir` (the only privileged step) is ever called, so it is provable
/// without root, unlike every other run-root assertion above.
#[tokio::test]
async fn a_run_root_path_too_long_for_sockaddr_un_is_refused_before_any_chown() {
    let (sock, _rec) = fake_daemon("running");
    let long_dir = format!("/tmp/wh-rr-{}", "x".repeat(90));
    let run_root = RunRoot::new(&long_dir).unwrap();
    let sb = sandbox_with_run_root(&sock, run_root);
    let id = Uuid::new_v4();
    let err = sb.provision(&id, &secrets()).await.unwrap_err();
    assert!(
        format!("{err:#}").contains("sockaddr_un") || format!("{err:#}").contains("100"),
        "{err:#}"
    );
    assert!(
        !std::path::Path::new(&format!("{long_dir}/{id}")).exists(),
        "the guard must fire before any directory is created"
    );
}

/// A container made without a run root, then reprovisioned once one is configured, must be
/// recreated rather than left alone: its `Binds` and `WHEEL_LISTEN` are stale relative to what the
/// proxy will now admit for this project.
#[tokio::test]
async fn enabling_a_run_root_on_an_existing_container_recreates_it() {
    require_root!();
    let (sock, rec) = fake_daemon("running");
    let id = Uuid::new_v4();

    {
        let sb = sandbox(&sock);
        sb.provision(&id, &secrets()).await.unwrap();
    }
    let (run_root, _path) = run_root_scratch();
    {
        let sb = sandbox_with_run_root(&sock, run_root);
        sb.provision(&id, &secrets()).await.unwrap();
    }

    let r = rec.lock().unwrap();
    let creates = r
        .requests
        .iter()
        .filter(|r| r.contains("/containers/create"))
        .count();
    assert_eq!(
        creates, 2,
        "turning on the run root must recreate the container, not silently keep the old bind"
    );
}
