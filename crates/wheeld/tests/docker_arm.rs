//! wheeld's docker sandbox arm, against a recording daemon behind the real filtering proxy.
//!
//! What matters here is the refusals and the restart behaviour, not the happy path: the arm must
//! not boot on anything it cannot prove is filtered, must not fall back, must adopt containers that
//! are already running without creating second ones, and must show a project that cannot be brought
//! back as an error rather than as a quietly stopped one.

use bollard::Docker;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use uuid::Uuid;
use wheel_host::docker_proxy::{policy::Policy, serve, Proxy};
use wheeld::config::SandboxMode;
use wheeld::supervise::Keys;

const VAULT_KEY: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";

/// Everything here reads and writes process-global environment (`DOCKER_HOST`, the defaults
/// `apply_defaults` composes), so the tests take turns.
fn env_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(tokio::sync::Mutex::default)
}

fn short(tag: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(format!(
        "/tmp/wd-{tag}-{}",
        &Uuid::new_v4().simple().to_string()[..8]
    ))
}

#[derive(Clone)]
struct Daemon {
    exists: bool,
    status: &'static str,
    health: Option<&'static str>,
    start_code: u16,
    /// The `wheel.spec` label of the last create, as a real daemon would remember it.
    spec: Option<String>,
    /// A raw daemon answers `GET /version`; the proxy never lets it get that far.
    requests: Vec<String>,
}

impl Daemon {
    fn absent() -> Self {
        Self {
            exists: false,
            status: "running",
            health: None,
            start_code: 204,
            spec: None,
            requests: vec![],
        }
    }
    fn running() -> Self {
        Self {
            exists: true,
            ..Self::absent()
        }
    }
}

fn fake_daemon(initial: Daemon) -> (std::path::PathBuf, Arc<Mutex<Daemon>>) {
    let path = short("sock");
    let listener = tokio::net::UnixListener::bind(&path).unwrap();
    let state = Arc::new(Mutex::new(initial));
    let shared = state.clone();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let shared = shared.clone();
            tokio::spawn(async move {
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
                let target = parts.next().unwrap_or("").to_string();
                let path = target.split('?').next().unwrap_or("").to_string();

                let (code, body) = {
                    let mut d = shared.lock().unwrap();
                    d.requests.push(format!("{method} {path}"));
                    if path.ends_with("/containers/json") {
                        (200, "[]".to_string())
                    } else if path.ends_with("/version") {
                        (
                            200,
                            r#"{"Version":"27.0.0","ApiVersion":"1.46"}"#.to_string(),
                        )
                    } else if path.ends_with("/containers/create") {
                        d.exists = true;
                        d.spec = serde_json::from_str::<serde_json::Value>(
                            raw.split("\r\n\r\n").nth(1).unwrap_or(""),
                        )
                        .ok()
                        .and_then(|b| b["Labels"]["wheel.spec"].as_str().map(str::to_string));
                        (201, r#"{"Id":"deadbeef","Warnings":[]}"#.to_string())
                    } else if path.ends_with("/volumes/create") {
                        (201, r#"{"Name":"v","Driver":"local","Mountpoint":"/m","Labels":{},"Scope":"local","Options":{}}"#.to_string())
                    } else if path.ends_with("/json") {
                        // The nil project is only ever the boot probe: no such container.
                        if d.exists && !path.contains(&Uuid::nil().to_string()) {
                            let health = d
                                .health
                                .map(|h| format!(r#","Health":{{"Status":"{h}"}}"#))
                                .unwrap_or_default();
                            // What a real daemon remembers of the create: its labels.
                            let labels = d
                                .spec
                                .as_ref()
                                .map(|h| {
                                    format!(r#","Config":{{"Labels":{{"wheel.spec":"{h}"}}}}"#)
                                })
                                .unwrap_or_default();
                            (
                                200,
                                format!(
                                    r#"{{"State":{{"Status":"{}"{health}}}{labels}}}"#,
                                    d.status
                                ),
                            )
                        } else {
                            (404, r#"{"message":"No such container"}"#.to_string())
                        }
                    } else if path.ends_with("/start") {
                        (
                            d.start_code,
                            if d.start_code >= 400 {
                                r#"{"message":"cannot start"}"#.to_string()
                            } else {
                                String::new()
                            },
                        )
                    } else {
                        (204, String::new())
                    }
                };
                let response = format!(
                    "HTTP/1.1 {code} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.flush().await;
            });
        }
    });
    (path, state)
}

async fn proxy_in_front_of(daemon: &std::path::Path) -> std::path::PathBuf {
    let listen = short("proxy");
    let proxy = Proxy::new(
        Policy {
            image: "wheel-engine:dev".into(),
            network: "wheel".into(),
            memory: 1024 * 1024 * 1024,
            nano_cpus: 1_000_000_000,
            pids_limit: 512,
            engine_port: 7000,
        },
        daemon.to_path_buf(),
    );
    let l = listen.clone();
    tokio::spawn(async move { serve(proxy, &l, 0o600, None).await.unwrap() });
    for _ in 0..200 {
        if listen.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    listen
}

/// A data dir whose host.db already holds `project`, wanted running — the state a restart finds.
async fn data_dir_with_running_project(project: Uuid) -> std::path::PathBuf {
    let dir = wheeld::supervise::prepare_data_dir(&short("data")).unwrap();
    let store = wheel_host::store::Store::open(&dir.join("host.db").display().to_string()).unwrap();
    store
        .upsert(&project, "engine-secret-of-this-project", VAULT_KEY)
        .await
        .unwrap();
    store.set_desired_running(&project, true).await.unwrap();
    dir
}

fn use_docker_host(sock: &std::path::Path) {
    std::env::set_var("DOCKER_HOST", format!("unix://{}", sock.display()));
    std::env::remove_var(wheeld::docker_arm::ENV_ALLOW_RAW_SOCKET);
    std::env::set_var("START_TIMEOUT_SECS", "1");
}

async fn boot(dir: &std::path::Path) -> anyhow::Result<wheeld::Host> {
    let keys = Keys::load_or_create(dir).unwrap();
    wheeld::start_host_with(dir, &keys, None, SandboxMode::Docker).await
}

async fn project_status(host: &wheeld::Host, id: Uuid) -> serde_json::Value {
    let secret = std::env::var("WHEEL_HOST_SECRET").unwrap();
    reqwest::Client::new()
        .get(format!("{}/host/v1/projects/{id}", host.url))
        .bearer_auth(secret)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

fn creates(d: &Arc<Mutex<Daemon>>) -> usize {
    d.lock()
        .unwrap()
        .requests
        .iter()
        .filter(|r| r.ends_with("/containers/create"))
        .count()
}

/// The one thing asserted from memory when this arm was planned: that bollard reads `DOCKER_HOST`.
/// Everything else here depends on it.
#[tokio::test]
async fn bollard_connects_to_the_socket_docker_host_names() {
    let _guard = env_lock().lock().await;
    let (sock, daemon) = fake_daemon(Daemon::running());
    std::env::set_var("DOCKER_HOST", format!("unix://{}", sock.display()));

    let docker = Docker::connect_with_local_defaults().unwrap();
    let _ = docker
        .inspect_container(
            "wheel-p-x",
            None::<bollard::query_parameters::InspectContainerOptions>,
        )
        .await;

    assert!(
        daemon
            .lock()
            .unwrap()
            .requests
            .iter()
            .any(|r| r.ends_with("/containers/wheel-p-x/json")),
        "DOCKER_HOST was not honoured; the daemon heard {:?}",
        daemon.lock().unwrap().requests
    );
}

#[tokio::test]
async fn docker_mode_refuses_the_daemons_own_socket_and_an_unset_host() {
    let _guard = env_lock().lock().await;
    std::env::remove_var(wheeld::docker_arm::ENV_ALLOW_RAW_SOCKET);
    for host in [
        None,
        Some("unix:///var/run/docker.sock"),
        Some("tcp://127.0.0.1:2375"),
    ] {
        match host {
            Some(h) => std::env::set_var("DOCKER_HOST", h),
            None => std::env::remove_var("DOCKER_HOST"),
        }
        let dir = wheeld::supervise::prepare_data_dir(&short("data")).unwrap();
        let err = boot(&dir).await.err().expect("must refuse to boot");
        let why = format!("{err:#}");
        assert!(
            why.contains("refusing to run the docker sandbox"),
            "{host:?}: {why}"
        );
    }
}

#[tokio::test]
async fn a_raw_daemon_on_an_innocent_path_is_still_refused() {
    let _guard = env_lock().lock().await;
    // Not the default path, so the address alone looks fine: it is what the socket ANSWERS that
    // gives it away.
    let (sock, daemon) = fake_daemon(Daemon::absent());
    use_docker_host(&sock);
    let dir = wheeld::supervise::prepare_data_dir(&short("data")).unwrap();

    let why = format!("{:#}", boot(&dir).await.err().expect("must refuse"));
    assert!(why.contains("did not identify itself"), "{why}");
    assert_eq!(creates(&daemon), 0);
}

#[tokio::test]
async fn the_filtering_proxy_is_accepted_and_a_project_gets_its_own_container() {
    let _guard = env_lock().lock().await;
    let (daemon_sock, daemon) = fake_daemon(Daemon::absent());
    let front = proxy_in_front_of(&daemon_sock).await;
    use_docker_host(&front);
    let dir = wheeld::supervise::prepare_data_dir(&short("data")).unwrap();
    let host = boot(&dir).await.expect("the proxy is a valid docker host");

    let id = Uuid::new_v4();
    let secret = std::env::var("WHEEL_HOST_SECRET").unwrap();
    let put = reqwest::Client::new()
        .put(format!("{}/host/v1/projects/{id}", host.url))
        .bearer_auth(secret)
        .json(&serde_json::json!({"engine_secret": "engine-secret-of-this-project", "vault_key": VAULT_KEY}))
        .send()
        .await
        .unwrap();
    assert_eq!(put.status(), 200, "{}", put.text().await.unwrap());
    assert_eq!(creates(&daemon), 1, "one project, one container");
}

/// A restart with containers already up must adopt them, not build second ones.
#[tokio::test]
async fn a_restart_reattaches_to_the_container_it_made_and_creates_nothing_new() {
    let _guard = env_lock().lock().await;
    let project = Uuid::new_v4();
    let (daemon_sock, daemon) = fake_daemon(Daemon::absent());
    let front = proxy_in_front_of(&daemon_sock).await;
    use_docker_host(&front);
    let dir = data_dir_with_running_project(project).await;

    // First boot builds the container. The second is the restart: same store, same daemon.
    let first = boot(&dir).await.unwrap();
    assert_eq!(creates(&daemon), 1);
    drop(first);
    let before = daemon.lock().unwrap().requests.len();

    let host = boot(&dir).await.unwrap();
    let after = daemon.lock().unwrap().requests[before..].to_vec();
    assert!(
        after
            .iter()
            .any(|r| r == &format!("POST /containers/wheel-p-{project}/start")),
        "the running project was never re-attached: {after:?}"
    );
    assert_eq!(
        creates(&daemon),
        1,
        "a second container was created: {after:?}"
    );
    assert_eq!(project_status(&host, project).await["status"], "running");
}

/// The survivor must not keep the secret it was born with: `provision` used to no-op once a
/// container existed, so a rotated secret (or a changed harness policy, or a new image) never
/// reached the engine, and a secret rotated BECAUSE it leaked stayed valid inside the container.
#[tokio::test]
async fn a_rotated_secret_recreates_the_container_on_its_volume() {
    let _guard = env_lock().lock().await;
    let project = Uuid::new_v4();
    let (daemon_sock, daemon) = fake_daemon(Daemon::absent());
    let front = proxy_in_front_of(&daemon_sock).await;
    use_docker_host(&front);
    let dir = data_dir_with_running_project(project).await;
    drop(boot(&dir).await.unwrap());
    assert_eq!(creates(&daemon), 1);

    let store = wheel_host::store::Store::open(&dir.join("host.db").display().to_string()).unwrap();
    store
        .upsert(&project, "a-different-engine-secret-now", VAULT_KEY)
        .await
        .unwrap();
    drop(boot(&dir).await.unwrap());

    let d = daemon.lock().unwrap().requests.clone();
    assert_eq!(creates(&daemon), 2, "the stale container was kept: {d:?}");
    assert!(
        d.iter()
            .any(|r| r.starts_with(&format!("DELETE /containers/wheel-p-{project}"))),
        "the stale container was not removed first: {d:?}"
    );
    assert!(
        !d.iter().any(|r| r.contains("DELETE /volumes")),
        "the volume — the project's data — must survive a recreate: {d:?}"
    );
}

/// A container removed out from under a project that should be running is rebuilt exactly once,
/// from its volume, rather than left as a hole.
#[tokio::test]
async fn a_missing_container_is_recreated_once_on_restart() {
    let _guard = env_lock().lock().await;
    let project = Uuid::new_v4();
    let (daemon_sock, daemon) = fake_daemon(Daemon::absent());
    let front = proxy_in_front_of(&daemon_sock).await;
    use_docker_host(&front);
    let dir = data_dir_with_running_project(project).await;

    let _host = boot(&dir).await.unwrap();
    assert_eq!(creates(&daemon), 1);
}

/// A container docker itself calls unhealthy is an error the operator can see, not a green project.
#[tokio::test]
async fn an_unhealthy_container_reads_as_an_error_with_its_reason() {
    let _guard = env_lock().lock().await;
    let project = Uuid::new_v4();
    let (daemon_sock, _) = fake_daemon(Daemon {
        health: Some("unhealthy"),
        ..Daemon::running()
    });
    let front = proxy_in_front_of(&daemon_sock).await;
    use_docker_host(&front);
    let dir = data_dir_with_running_project(project).await;

    let host = boot(&dir).await.unwrap();
    let status = project_status(&host, project).await;
    assert_eq!(status["status"], "error", "{status}");
    assert!(
        status["last_error"].as_str().unwrap().contains("unhealthy"),
        "{status}"
    );
}

/// A project that could not be brought back on restart is an error with the reason, not "stopped".
#[tokio::test]
async fn a_project_that_failed_to_come_back_is_an_error_not_a_quiet_stop() {
    let _guard = env_lock().lock().await;
    let project = Uuid::new_v4();
    let (daemon_sock, _) = fake_daemon(Daemon {
        status: "exited",
        start_code: 500,
        ..Daemon::running()
    });
    let front = proxy_in_front_of(&daemon_sock).await;
    use_docker_host(&front);
    let dir = data_dir_with_running_project(project).await;

    let host = boot(&dir).await.unwrap();
    let status = project_status(&host, project).await;
    assert_eq!(status["status"], "error", "{status}");
    assert!(
        status["last_error"]
            .as_str()
            .unwrap()
            .contains("failed to start"),
        "{status}"
    );
}

struct NoUpdates;
impl wheel_engine::update::UpdateHook for NoUpdates {
    fn notice(&self) -> Option<wheel_core::UpdateNotice> {
        None
    }
    fn request(&self, _: wheel_engine::update::Requester) -> wheel_engine::update::RequestOutcome {
        unreachable!("never asked")
    }
    fn attach(&self, _: Uuid, _: std::sync::Weak<dyn wheel_engine::update::EngineControl>) {}
}

#[tokio::test]
async fn docker_mode_refuses_the_update_lane_by_name() {
    let _guard = env_lock().lock().await;
    let (daemon_sock, _) = fake_daemon(Daemon::absent());
    let front = proxy_in_front_of(&daemon_sock).await;
    use_docker_host(&front);
    let dir = wheeld::supervise::prepare_data_dir(&short("data")).unwrap();
    let keys = Keys::load_or_create(&dir).unwrap();

    let hook: Arc<dyn wheel_engine::update::UpdateHook> = Arc::new(NoUpdates);
    let err = wheeld::start_host_with(&dir, &keys, Some(hook), SandboxMode::Docker)
        .await
        .err()
        .expect("must refuse");
    let why = format!("{err:#}");
    assert!(
        why.contains("WHEEL_AUTO_UPDATE") && why.contains("docker compose pull"),
        "{why}"
    );
}

#[tokio::test]
async fn a_data_directory_cannot_change_sandbox_kind_under_its_projects() {
    let _guard = env_lock().lock().await;
    let (daemon_sock, _) = fake_daemon(Daemon::absent());
    let front = proxy_in_front_of(&daemon_sock).await;
    use_docker_host(&front);
    let dir = wheeld::supervise::prepare_data_dir(&short("data")).unwrap();
    drop(boot(&dir).await.unwrap());

    let keys = Keys::load_or_create(&dir).unwrap();
    std::env::remove_var("SANDBOX_BACKEND");
    let why = format!(
        "{:#}",
        wheeld::start_host_with(&dir, &keys, None, SandboxMode::Embedded)
            .await
            .err()
            .expect("switching kinds must be refused")
    );
    assert!(why.contains("belongs to the docker sandbox"), "{why}");
}

#[tokio::test]
async fn the_two_spellings_of_the_sandbox_setting_must_agree() {
    let _guard = env_lock().lock().await;
    let (daemon_sock, _) = fake_daemon(Daemon::absent());
    let front = proxy_in_front_of(&daemon_sock).await;
    use_docker_host(&front);
    let dir = wheeld::supervise::prepare_data_dir(&short("data")).unwrap();

    std::env::set_var("SANDBOX_BACKEND", "process");
    let why = format!("{:#}", boot(&dir).await.err().expect("must refuse"));
    std::env::remove_var("SANDBOX_BACKEND");
    assert!(why.contains("SANDBOX_BACKEND=process"), "{why}");
}

#[tokio::test]
async fn the_raw_socket_flag_is_ignored_outside_a_dev_environment() {
    let _guard = env_lock().lock().await;
    let (daemon_sock, _) = fake_daemon(Daemon::absent());
    use_docker_host(&daemon_sock);
    std::env::set_var(wheeld::docker_arm::ENV_ALLOW_RAW_SOCKET, "1");
    let dir = wheeld::supervise::prepare_data_dir(&short("data")).unwrap();

    std::env::set_var("WHEEL_ENV", "prod");
    let why = format!("{:#}", boot(&dir).await.err().expect("must refuse in prod"));
    assert!(why.contains("WHEEL_ENV=dev"), "{why}");

    std::env::set_var("WHEEL_ENV", "dev");
    let dir = wheeld::supervise::prepare_data_dir(&short("data")).unwrap();
    boot(&dir)
        .await
        .expect("the dev opt-in works against a raw daemon");

    std::env::remove_var(wheeld::docker_arm::ENV_ALLOW_RAW_SOCKET);
    std::env::set_var("WHEEL_ENV", "prod");
}
