// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

//! The docker socket proxy, in front of the REAL docker backend and a recording daemon.
//!
//! The policy's unit tests use a hand-written "golden" create body. This is what stops that being a
//! copy that drifts: `DockerSandbox` itself is pointed at the proxy and driven through its whole
//! lifecycle, so if the backend ever asks for something the policy does not admit — or the policy is
//! tightened past what the backend asks for — a test here fails rather than a deploy.

use bollard::Docker;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use uuid::Uuid;
use wheel_host::config::{Backend, Config};
use wheel_host::docker_proxy::{policy::Policy, serve, Proxy};
use wheel_host::sandbox::{docker::DockerSandbox, Sandbox, Secrets, Status};

const IMAGE: &str = "wheel-engine:test";
const NETWORK: &str = "wheel-tenants";
const SECRET: &str = "engine-secret-value-s3cret";

fn sock(tag: &str) -> std::path::PathBuf {
    let p = std::path::PathBuf::from(format!(
        "/tmp/wh-{tag}-{}.sock",
        &Uuid::new_v4().simple().to_string()[..8]
    ));
    let _ = std::fs::remove_file(&p);
    p
}

/// A docker daemon that records every request line it is sent. Inspection reports the container's
/// environment, as a real one does, so a test can see whether the proxy removes it.
fn fake_daemon() -> (std::path::PathBuf, Arc<Mutex<Vec<String>>>) {
    fake_daemon_with(false)
}

/// `pre_existing` starts the daemon as if the container were already there.
fn fake_daemon_with(pre_existing: bool) -> (std::path::PathBuf, Arc<Mutex<Vec<String>>>) {
    let path = sock("daemon");
    let listener = tokio::net::UnixListener::bind(&path).unwrap();
    let seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let recorder = seen.clone();
    let created = Arc::new(std::sync::atomic::AtomicBool::new(pre_existing));
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let recorder = recorder.clone();
            let created = created.clone();
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
                recorder.lock().unwrap().push(format!("{method} {target}"));
                let path = target.split('?').next().unwrap_or("");

                if path.ends_with("/containers/create") {
                    created.store(true, std::sync::atomic::Ordering::SeqCst);
                }
                let exists = created.load(std::sync::atomic::Ordering::SeqCst);
                let (code, body) = if path.ends_with("/containers/json") {
                    (200, "[]".to_string())
                } else if path.ends_with("/json") && !exists {
                    // A real daemon 404s until the container exists; without this `provision`
                    // always thinks it is already there and no test ever sees a create.
                    (404, r#"{"message":"No such container"}"#.to_string())
                } else if path.ends_with("/json") {
                    (
                        200,
                        format!(
                            r#"{{"State":{{"Status":"running"}},"Config":{{"Image":"{IMAGE}","Env":["WHEEL_ENGINE_SECRET={SECRET}"]}}}}"#
                        ),
                    )
                } else if path.ends_with("/volumes/create") {
                    (201, r#"{"Name":"v","Driver":"local","Mountpoint":"/m","Labels":{},"Scope":"local","Options":{}}"#.into())
                } else if path.ends_with("/containers/create") {
                    (201, r#"{"Id":"deadbeef","Warnings":[]}"#.into())
                } else {
                    (204, String::new())
                };
                let response = format!(
                    "HTTP/1.1 {code} OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.flush().await;
            });
        }
    });
    (path, seen)
}

fn policy() -> Policy {
    Policy {
        image: IMAGE.into(),
        network: NETWORK.into(),
        memory: 512 * 1024 * 1024,
        nano_cpus: 1_500_000_000,
        pids_limit: 256,
        engine_port: 7000,
        run_root: None,
        max_projects: 0,
    }
}

/// The proxy in front of `upstream`; returns the socket a client connects to.
async fn proxy_in_front_of(upstream: &std::path::Path) -> std::path::PathBuf {
    let listen = sock("proxy");
    let proxy = Proxy::new(policy(), upstream.to_path_buf());
    let l = listen.clone();
    tokio::spawn(async move { serve(proxy, &l, 0o600, None).await.unwrap() });
    for _ in 0..100 {
        if listen.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    listen
}

fn cfg() -> Config {
    Config {
        bind_addr: "127.0.0.1:0".into(),
        secret: "test-host-secret-at-least-16".into(),
        backend: Backend::Docker,
        data_dir: "/tmp/wheel-docker-proxy-test".into(),
        engine_image: IMAGE.into(),
        docker_network: NETWORK.into(),
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

fn secrets() -> Secrets {
    Secrets {
        engine_secret: SECRET.into(),
        vault_key: "dmF1bHQta2V5LTMyLWJ5dGVzLWV4YWN0bHktb2sh".into(),
    }
}

/// One HTTP/1.1 request over a unix socket, answer read to the end. Returns (status, body).
async fn raw(sock: &std::path::Path, method: &str, target: &str, body: &str) -> (u16, String) {
    let mut s = tokio::net::UnixStream::connect(sock).await.unwrap();
    let req = format!(
        "{method} {target} HTTP/1.1\r\nhost: docker\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    s.write_all(req.as_bytes()).await.unwrap();
    let mut out = Vec::new();
    let _ = s.read_to_end(&mut out).await;
    let text = String::from_utf8_lossy(&out).to_string();
    let status = text
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    let body = text.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
    (status, body)
}

#[tokio::test]
async fn the_real_docker_backend_runs_its_whole_lifecycle_through_the_proxy() {
    let (daemon, seen) = fake_daemon();
    let front = proxy_in_front_of(&daemon).await;
    let docker =
        Docker::connect_with_unix(front.to_str().unwrap(), 5, bollard::API_DEFAULT_VERSION)
            .unwrap();
    let sb = DockerSandbox::with_client(docker, cfg());
    let id = Uuid::new_v4();

    // `start` provisions, starts the container, then waits for an engine that is not there; it is
    // the wait that fails, and it must be the only thing that does.
    let started = sb.start(&id, &secrets()).await;
    let why = format!("{:#}", started.expect_err("no engine is listening"));
    assert!(
        why.contains("did not become healthy"),
        "start failed for the wrong reason: {why}"
    );

    assert_eq!(sb.status(&id).await.unwrap(), Status::Running);
    sb.stop(&id).await.unwrap();
    sb.destroy(&id).await.unwrap();

    let c = format!("wheel-p-{id}");
    let v = format!("wheel-p-{id}-data");
    let strip = |s: &String| {
        let (m, t) = s.split_once(' ').unwrap();
        // Only a real version prefix (/v1.49/...): stripping a bare "/v" also eats "/volumes".
        let t = match t.strip_prefix("/v") {
            Some(r) if r.starts_with(|c: char| c.is_ascii_digit()) => {
                r.find('/').map_or(t.to_string(), |i| r[i..].to_string())
            }
            _ => t.to_string(),
        };
        format!("{m} {t}")
    };
    let calls: Vec<String> = seen.lock().unwrap().iter().map(strip).collect();
    for expected in [
        "POST /volumes/create".to_string(),
        format!("POST /containers/create?name={c}"),
        format!("POST /containers/{c}/start"),
        format!("POST /containers/{c}/stop?t=30"),
        format!("DELETE /containers/{c}?v=false&force=true"),
        format!("DELETE /volumes/{v}?force=true"),
    ] {
        assert!(
            calls
                .iter()
                .any(|call| call.starts_with(expected.split('?').next().unwrap())),
            "the daemon never heard {expected}; it heard {calls:#?}"
        );
    }
}

#[tokio::test]
async fn an_inspection_never_carries_the_engine_secret_back() {
    let (daemon, _) = fake_daemon_with(true);
    let front = proxy_in_front_of(&daemon).await;
    let id = Uuid::new_v4();

    let (status, body) = raw(
        &front,
        "GET",
        &format!("/v1.49/containers/wheel-p-{id}/json"),
        "",
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("running"), "the state must survive: {body}");
    assert!(
        !body.contains(SECRET) && !body.contains("WHEEL_ENGINE_SECRET"),
        "{body}"
    );
}

#[tokio::test]
async fn a_hostile_create_is_refused_and_the_daemon_never_hears_it() {
    let (daemon, seen) = fake_daemon();
    let front = proxy_in_front_of(&daemon).await;
    let id = Uuid::new_v4();
    let body = serde_json::json!({
        "Image": IMAGE,
        "HostConfig": { "Privileged": true, "Binds": ["/:/host"] },
    })
    .to_string();

    let (status, answer) = raw(
        &front,
        "POST",
        &format!("/containers/create?name=wheel-p-{id}"),
        &body,
    )
    .await;
    assert_eq!(status, 403, "{answer}");
    assert!(answer.contains("refused"), "{answer}");
    assert!(
        seen.lock().unwrap().is_empty(),
        "the daemon was reached: {:?}",
        seen.lock().unwrap()
    );
}

#[tokio::test]
async fn calls_the_host_never_makes_do_not_reach_the_daemon() {
    let (daemon, seen) = fake_daemon();
    let front = proxy_in_front_of(&daemon).await;
    for (m, t) in [
        ("GET", "/containers/json"),
        ("GET", "/info"),
        ("POST", "/build"),
        ("POST", "/containers/anything/exec"),
        ("POST", "/networks/create"),
    ] {
        let (status, _) = raw(&front, m, t, "{}").await;
        assert_eq!(status, 403, "{m} {t}");
    }
    assert!(
        seen.lock().unwrap().is_empty(),
        "{:?}",
        seen.lock().unwrap()
    );
}

#[tokio::test]
async fn an_unreachable_daemon_is_a_gateway_error_not_a_hang() {
    let front = proxy_in_front_of(std::path::Path::new("/tmp/wh-no-such-daemon.sock")).await;
    let id = Uuid::new_v4();
    let (status, body) = raw(
        &front,
        "POST",
        &format!("/containers/wheel-p-{id}/start"),
        "",
    )
    .await;
    assert_eq!(status, 502, "{body}");
}

#[tokio::test]
async fn the_proxy_says_what_it_is_and_never_forwards_that_question() {
    let (daemon, seen) = fake_daemon();
    let front = proxy_in_front_of(&daemon).await;
    let (status, body) = raw(&front, "GET", "/_wheel_proxy", "").await;
    assert_eq!(status, 200);
    assert_eq!(body, wheel_host::docker_proxy::IDENTITY_BODY);
    assert!(
        seen.lock().unwrap().is_empty(),
        "the identity check reached the daemon"
    );
}

#[tokio::test]
async fn the_project_container_list_is_admitted_and_reduced() {
    let (daemon, seen) = fake_daemon();
    let front = proxy_in_front_of(&daemon).await;
    let target =
        "/v1.49/containers/json?all=true&filters=%7B%22label%22%3A%5B%22wheel.project%22%5D%7D";
    let (status, _) = raw(&front, "GET", target, "").await;
    assert_eq!(status, 200);
    let heard = seen.lock().unwrap().clone();
    assert_eq!(heard.len(), 1, "{heard:?}");
    assert!(
        heard[0].contains("filters=%7B%22label%22%3A%5B%22wheel.project%22%5D%7D"),
        "{heard:?}"
    );
}

/// Headers that change what a request IS — a connection upgrade, a deferred body, a chunked body —
/// are refused before the policy runs, and the daemon never hears of the request. Pinned because the
/// refusal is defence in depth: nothing else would fail if it were removed.
#[tokio::test]
async fn headers_that_change_what_a_request_is_are_refused_before_the_daemon_hears_it() {
    let (daemon, seen) = fake_daemon_with(true);
    let front = proxy_in_front_of(&daemon).await;
    let id = Uuid::new_v4();
    for (header, body) in [
        ("upgrade: websocket", ""),
        ("expect: 100-continue", ""),
        ("transfer-encoding: chunked", "0\r\n\r\n"),
    ] {
        let mut s = tokio::net::UnixStream::connect(&front).await.unwrap();
        let req = format!(
            "GET /containers/wheel-p-{id}/json HTTP/1.1\r\nhost: docker\r\n{header}\r\nconnection: close\r\n\r\n{body}"
        );
        s.write_all(req.as_bytes()).await.unwrap();
        let mut out = Vec::new();
        let _ = s.read_to_end(&mut out).await;
        let text = String::from_utf8_lossy(&out);
        assert!(
            text.starts_with("HTTP/1.1 403"),
            "{header} was not refused: {text}"
        );
    }
    assert!(
        seen.lock().unwrap().is_empty(),
        "the daemon heard a refused request: {:?}",
        seen.lock().unwrap()
    );
}

// --------------------------------------------------------------------- project ceiling (M3a)

/// A daemon that answers a fixed number of project containers on `/containers/json` and admits
/// creates otherwise, so the ceiling is the only thing under test.
fn fake_daemon_listing(count: usize) -> std::path::PathBuf {
    let path = sock("daemon-list");
    let listener = tokio::net::UnixListener::bind(&path).unwrap();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
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
                let target = raw.split_whitespace().nth(1).unwrap_or("").to_string();
                let path = target.split('?').next().unwrap_or("");
                let (code, body) = if path.ends_with("/containers/json") {
                    let items: Vec<String> = (0..count)
                        .map(|_| {
                            format!(
                                r#"{{"Names":["/wheel-p-{}"],"State":"running","Labels":{{"wheel.project":"{}"}}}}"#,
                                Uuid::new_v4(),
                                Uuid::new_v4()
                            )
                        })
                        .collect();
                    (200, format!("[{}]", items.join(",")))
                } else if path.ends_with("/volumes") {
                    // No volumes of their own in this fixture: the `count` existing projects are
                    // represented by containers alone, which is enough to prove the ceiling reacts
                    // to the count — a SEPARATE fixture (`fake_daemon_listing_volumes_only`, if a
                    // future test needs it) would represent the volume-only-orphan case instead.
                    (200, r#"{"Volumes":[],"Warnings":[]}"#.to_string())
                } else if path.ends_with("/volumes/create") {
                    (201, r#"{"Name":"v","Driver":"local","Mountpoint":"/m","Labels":{},"Scope":"local","Options":{}}"#.to_string())
                } else if path.ends_with("/containers/create") {
                    (201, r#"{"Id":"deadbeef","Warnings":[]}"#.to_string())
                } else {
                    (204, String::new())
                };
                let response = format!(
                    "HTTP/1.1 {code} OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.flush().await;
            });
        }
    });
    path
}

async fn proxy_with_ceiling(daemon: &std::path::Path, max: usize) -> std::path::PathBuf {
    let listen = sock("proxy-ceil");
    let mut p = policy();
    p.max_projects = max;
    let proxy = Proxy::new(p, daemon.to_path_buf());
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

#[tokio::test]
async fn a_create_past_the_ceiling_is_refused_and_never_reaches_the_daemon() {
    let daemon = fake_daemon_listing(3);
    let front = proxy_with_ceiling(&daemon, 3).await;
    let id = Uuid::new_v4();
    let (status, body) = raw(
        &front,
        "POST",
        &format!("/containers/create?name=wheel-p-{id}"),
        &golden(&id),
    )
    .await;
    assert_eq!(status, 403, "{body}");
    assert!(body.to_lowercase().contains("ceiling"), "{body}");
}

#[tokio::test]
async fn a_create_under_the_ceiling_is_admitted() {
    let daemon = fake_daemon_listing(2);
    let front = proxy_with_ceiling(&daemon, 3).await;
    let id = Uuid::new_v4();
    let (status, body) = raw(
        &front,
        "POST",
        &format!("/containers/create?name=wheel-p-{id}"),
        &golden(&id),
    )
    .await;
    assert_eq!(status, 201, "{body}");
}

#[tokio::test]
async fn a_zero_ceiling_means_no_ceiling() {
    let daemon = fake_daemon_listing(1_000_000);
    let front = proxy_with_ceiling(&daemon, 0).await;
    let id = Uuid::new_v4();
    let (status, body) = raw(
        &front,
        "POST",
        &format!("/containers/create?name=wheel-p-{id}"),
        &golden(&id),
    )
    .await;
    assert_eq!(status, 201, "{body}");
}

/// The daemon this counts against is the SAME upstream the create would use; a daemon that cannot
/// even answer the list must not become a way to block every future create.
#[tokio::test]
async fn a_daemon_that_cannot_be_asked_does_not_block_creates() {
    let front = proxy_with_ceiling(
        std::path::Path::new("/tmp/wh-no-daemon-for-ceiling.sock"),
        3,
    )
    .await;
    let id = Uuid::new_v4();
    let (status, _) = raw(
        &front,
        "POST",
        &format!("/containers/create?name=wheel-p-{id}"),
        &golden(&id),
    )
    .await;
    // Refused for lack of a daemon (502), not for the ceiling (403) -- the create was still
    // ATTEMPTED, which is the property under test.
    assert_eq!(status, 502);
}

/// `golden()`'s helper takes the FIXED test `id()`; the ceiling tests each use a fresh project id,
/// so this builds the same shape for an arbitrary one.
fn golden(id: &Uuid) -> String {
    serde_json::json!({
        "Image": IMAGE,
        "Env": [
            format!("WHEEL_PROJECT_ID={id}"),
            "WHEEL_ENGINE_SECRET=engine-secret",
            "WHEEL_VAULT_KEY=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
            "WHEEL_HARNESS_AUTH=api-key-only",
            "WHEEL_LISTEN=tcp://0.0.0.0:7000",
            "WHEEL_DATA_DIR=/data",
            "WHEEL_LOG=json",
            "WHEEL_ROLE=engine",
        ],
        "Labels": { "wheel.project": id.to_string() },
        "HostConfig": {
            "CapDrop": ["ALL"],
            "SecurityOpt": ["no-new-privileges"],
            "Memory": 512 * 1024 * 1024i64,
            "MemorySwap": 512 * 1024 * 1024i64,
            "NanoCpus": 1_500_000_000i64,
            "PidsLimit": 256,
            "NetworkMode": NETWORK,
            "Binds": [format!("wheel-p-{id}-data:/data")],
            "RestartPolicy": { "Name": "unless-stopped" },
        },
    })
    .to_string()
}

/// A daemon that reports `count` VOLUMES but no containers at all — adversary's Gap A shape
/// (`POST /volumes/create` in a loop, never paired with a container).
fn fake_daemon_volumes_only(count: usize) -> std::path::PathBuf {
    let path = sock("daemon-vol-only");
    let listener = tokio::net::UnixListener::bind(&path).unwrap();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
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
                let target = raw.split_whitespace().nth(1).unwrap_or("").to_string();
                let path = target.split('?').next().unwrap_or("");
                let (code, body) = if path.ends_with("/containers/json") {
                    (200, "[]".to_string())
                } else if path.ends_with("/volumes") {
                    let items: Vec<String> = (0..count)
                        .map(|_| {
                            let id = Uuid::new_v4();
                            format!(
                                r#"{{"Name":"wheel-p-{id}-data","Driver":"local","Mountpoint":"","Labels":{{"wheel.project":"{id}"}}}}"#
                            )
                        })
                        .collect();
                    (
                        200,
                        format!(r#"{{"Volumes":[{}],"Warnings":[]}}"#, items.join(",")),
                    )
                } else if path.ends_with("/volumes/create") {
                    (201, r#"{"Name":"v","Driver":"local","Mountpoint":"/m","Labels":{},"Scope":"local","Options":{}}"#.to_string())
                } else {
                    (204, String::new())
                };
                let response = format!(
                    "HTTP/1.1 {code} OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.flush().await;
            });
        }
    });
    path
}

/// Adversary's Gap A, reproduced against the fix: without volumes counted, this admitted 50/50
/// volume creates against a ceiling of 2. With them counted, the ceiling refuses once the volume
/// count alone reaches it — `POST /volumes/create` is capped even though no container ever exists.
#[tokio::test]
async fn a_flood_of_volume_only_creates_is_capped_by_the_ceiling() {
    let daemon = fake_daemon_volumes_only(2);
    let front = proxy_with_ceiling(&daemon, 2).await;
    let id = Uuid::new_v4();
    let (status, body) = raw(
        &front,
        "POST",
        "/volumes/create",
        &serde_json::json!({"Name": format!("wheel-p-{id}-data"), "Labels": {"wheel.project": id.to_string()}}).to_string(),
    )
    .await;
    assert_eq!(status, 403, "{body}");
    assert!(body.to_lowercase().contains("ceiling"), "{body}");
}

/// The race adversary demonstrated: without the create-serialising lock, a slow-listing daemon let
/// more than one of ten concurrent creates through a ceiling of one. With it, at most one admitted.
#[tokio::test]
async fn concurrent_creates_against_a_slow_daemon_do_not_defeat_the_ceiling() {
    let daemon = fake_daemon_slow_listing(150);
    let front = std::sync::Arc::new(proxy_with_ceiling(&daemon, 1).await);
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..10 {
        let front = front.clone();
        tasks.spawn(async move {
            let id = Uuid::new_v4();
            let (status, _) = raw(
                &front,
                "POST",
                &format!("/containers/create?name=wheel-p-{id}"),
                &golden(&id),
            )
            .await;
            status
        });
    }
    let mut admitted = 0;
    while let Some(r) = tasks.join_next().await {
        if r.unwrap() == 201 {
            admitted += 1;
        }
    }
    assert_eq!(
        admitted, 1,
        "the ceiling let more than one concurrent create through"
    );
}

/// A daemon whose `/containers/json` (the ceiling's count) is slow — long enough to open the race
/// window a serialising lock has to close — and otherwise empty, so anything admitted is a bug.
fn fake_daemon_slow_listing(delay_ms: u64) -> std::path::PathBuf {
    let path = sock("daemon-slow");
    let listener = tokio::net::UnixListener::bind(&path).unwrap();
    // STATEFUL: a create must actually grow what the next list call reports, or the race this
    // daemon exists to open is invisible — every concurrent count would see the same "0 so far"
    // and admitting all of them would look identical to admitting only the first.
    let count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let count = count.clone();
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
                let target = raw.split_whitespace().nth(1).unwrap_or("").to_string();
                let path = target.split('?').next().unwrap_or("");
                if path.ends_with("/containers/json") {
                    tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                }
                let (code, body) = if path.ends_with("/containers/json") {
                    let items: Vec<String> = (0..count.load(std::sync::atomic::Ordering::SeqCst))
                        .map(|_| {
                            let id = Uuid::new_v4();
                            format!(
                                r#"{{"Names":["/wheel-p-{id}"],"State":"running","Labels":{{"wheel.project":"{id}"}}}}"#
                            )
                        })
                        .collect();
                    (200, format!("[{}]", items.join(",")))
                } else if path.ends_with("/volumes") {
                    (200, r#"{"Volumes":[],"Warnings":[]}"#.to_string())
                } else if path.ends_with("/containers/create") {
                    count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    (201, r#"{"Id":"deadbeef","Warnings":[]}"#.to_string())
                } else {
                    (204, String::new())
                };
                let response = format!(
                    "HTTP/1.1 {code} OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.flush().await;
            });
        }
    });
    path
}
