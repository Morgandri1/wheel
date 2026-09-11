// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

//! The headless promise, end to end, against the real binary.
//!
//! A fresh data directory and nothing else. The daemon must come up with an operator token nobody
//! had to log in for, and must write that token only to its 0600 file, never to its log. The token
//! must work, `wheeld token` must manage it, a revoked one must stop working, and a request
//! addressed to a stranger's host name must be refused.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn wheeld(args: &[&str], env: &[(&str, &str)]) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_wheeld"));
    cmd.args(args)
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", std::env::var("HOME").unwrap_or_default());
    for (k, v) in env {
        cmd.env(k, v);
    }
    // See tests/shutdown.rs: without this the subprocess is invisible to coverage.
    if let Ok(profile) = std::env::var("LLVM_PROFILE_FILE") {
        cmd.env("LLVM_PROFILE_FILE", profile);
    }
    cmd
}

struct Daemon {
    child: Child,
    base: String,
    log: PathBuf,
}

impl Daemon {
    fn start(dir: &Path, env: &[(&str, &str)]) -> Self {
        let port = free_port();
        let log = dir.with_extension("log");
        let mut child = wheeld(
            &[
                "--data-dir",
                &dir.display().to_string(),
                "--bind",
                &format!("127.0.0.1:{port}"),
            ],
            env,
        )
        .stdout(Stdio::from(std::fs::File::create(&log).unwrap()))
        .stderr(Stdio::null())
        .spawn()
        .expect("wheeld starts");
        let base = format!("http://127.0.0.1:{port}");
        let deadline = Instant::now() + Duration::from_secs(60);
        while Instant::now() < deadline {
            if reqwest::blocking::get(format!("{base}/healthz"))
                .is_ok_and(|r| r.status().is_success())
            {
                return Self { child, base, log };
            }
            if let Ok(Some(status)) = child.try_wait() {
                panic!(
                    "wheeld exited during boot ({status}): {}",
                    std::fs::read_to_string(&log).unwrap_or_default()
                );
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        child.kill().ok();
        child.wait().ok();
        panic!("wheeld never served {base}/healthz");
    }

    fn status(&self, path: &str, headers: &[(&str, &str)]) -> u16 {
        let mut req = reqwest::blocking::Client::new().get(format!("{}{path}", self.base));
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        req.send().unwrap().status().as_u16()
    }

    fn stop(mut self) -> String {
        unsafe { libc::kill(self.child.id() as i32, libc::SIGTERM) };
        self.child.wait().unwrap();
        std::fs::read_to_string(&self.log).unwrap()
    }
}

fn data_dir(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("wheeld-{tag}-{}", uuid::Uuid::new_v4().simple()))
}

fn token_cli(dir: &Path, args: &[&str]) -> (i32, String, String) {
    let mut all = vec!["token"];
    all.extend_from_slice(args);
    let dir = dir.display().to_string();
    all.extend_from_slice(&["--data-dir", &dir]);
    let out = wheeld(&all, &[]).output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn a_fresh_data_dir_boots_headless_with_a_working_operator_token() {
    use std::os::unix::fs::PermissionsExt;
    let dir = data_dir("headless");
    let daemon = Daemon::start(&dir, &[]);

    let file = dir.join("operator-token");
    let mode = std::fs::metadata(&file)
        .expect("operator-token was written")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600, "operator-token is {mode:o}");
    let operator = std::fs::read_to_string(&file).unwrap().trim().to_string();
    assert!(operator.starts_with("wht_"), "{operator}");

    assert_eq!(
        daemon.status("/v1/projects", &[("x-auth-token", &operator)]),
        200
    );
    let bearer = format!("Bearer {operator}");
    assert_eq!(
        daemon.status("/v1/projects", &[("authorization", &bearer)]),
        200
    );
    assert_eq!(daemon.status("/v1/projects", &[]), 401);

    // With no PUBLIC_BASE_URL, ingress URLs name this daemon the way every earlier install did.
    let project: serde_json::Value = reqwest::blocking::Client::new()
        .post(format!("{}/v1/projects", daemon.base))
        .header("x-auth-token", &operator)
        .json(&serde_json::json!({"name": "headless"}))
        .send()
        .unwrap()
        .json()
        .unwrap();
    let port = daemon.base.rsplit(':').next().unwrap();
    assert_eq!(
        project["ingress_base_url"],
        format!(
            "http://localhost:{port}/p/{}",
            project["id"].as_str().unwrap()
        )
    );

    // Reachable from this machine only, signup stays open: a laptop user is never locked out.
    let signup = reqwest::blocking::Client::new()
        .post(format!("{}/v1/auth/signup", daemon.base))
        .json(&serde_json::json!({"email": "local@example.test", "password": "Correct-Horse-9!"}))
        .send()
        .unwrap();
    assert_eq!(signup.status().as_u16(), 201);

    // DNS rebinding: the page's own name arrives as the Host, and is refused.
    assert_eq!(
        daemon.status(
            "/v1/projects",
            &[("x-auth-token", &operator), ("host", "evil.example")]
        ),
        403
    );

    let (rc, second, _) = token_cli(&dir, &["create", "--name", "second"]);
    assert_eq!(rc, 0);
    let second = second.trim().to_string();
    assert_eq!(
        daemon.status("/v1/projects", &[("x-auth-token", &second)]),
        200
    );

    let (rc, listing, _) = token_cli(&dir, &["list"]);
    assert_eq!(rc, 0);
    let id = listing
        .lines()
        .find(|l| l.contains(" operator "))
        .and_then(|l| l.split_whitespace().next())
        .expect("the operator token is listed")
        .to_string();
    let (rc, _, said) = token_cli(&dir, &["revoke", &id]);
    assert_eq!(rc, 0, "{said}");

    assert_eq!(
        daemon.status("/v1/projects", &[("x-auth-token", &operator)]),
        401,
        "a revoked token still works"
    );
    assert_eq!(
        daemon.status("/v1/projects", &[("x-auth-token", &second)]),
        200,
        "revoking one token ended another"
    );

    let log = daemon.stop();
    assert!(
        log.contains(&file.display().to_string()),
        "the log does not name the token file:\n{log}"
    );
    assert!(!log.contains("wht_"), "a token reached the log:\n{log}");
    assert!(
        !log.contains("Anyone who can route to it"),
        "a loopback bind was warned about:\n{log}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn an_exposed_bind_says_so_and_a_closed_signup_is_closed() {
    let dir = data_dir("exposed");
    let port = free_port();
    let log = dir.with_extension("log");
    let mut child = wheeld(
        &[
            "--data-dir",
            &dir.display().to_string(),
            "--bind",
            &format!("0.0.0.0:{port}"),
        ],
        &[
            ("PUBLIC_BASE_URL", "https://wheel.example"),
            ("WHEEL_TRUSTED_PROXIES", "127.0.0.1/32"),
        ],
    )
    .stdout(Stdio::from(std::fs::File::create(&log).unwrap()))
    .stderr(Stdio::null())
    .spawn()
    .unwrap();
    let base = format!("http://127.0.0.1:{port}");
    let deadline = Instant::now() + Duration::from_secs(60);
    while !reqwest::blocking::get(format!("{base}/healthz")).is_ok_and(|r| r.status().is_success())
    {
        assert!(Instant::now() < deadline, "wheeld never came up");
        std::thread::sleep(Duration::from_millis(100));
    }

    let signup = reqwest::blocking::Client::new()
        .post(format!("{base}/v1/auth/signup"))
        .json(&serde_json::json!({"email": "late@example.test", "password": "Correct-Horse-9!"}))
        .send()
        .unwrap();
    assert_eq!(
        signup.status().as_u16(),
        403,
        "an exposed wheeld must close signup without being told"
    );

    // The owner still lets people in, with the operator token, and they can then sign in.
    let operator = std::fs::read_to_string(dir.join("operator-token")).unwrap();
    let client = reqwest::blocking::Client::new();
    let person =
        serde_json::json!({"email": "invited@example.test", "password": "Correct-Horse-9!"});
    let added = client
        .post(format!("{base}/v1/auth/users"))
        .header("x-auth-token", operator.trim())
        .json(&person)
        .send()
        .unwrap();
    assert_eq!(added.status().as_u16(), 201);
    let login = client
        .post(format!("{base}/v1/auth/login"))
        .json(&person)
        .send()
        .unwrap();
    assert_eq!(login.status().as_u16(), 200);

    // Behind a proxy, ingress URLs name the public address the operator configured.
    let operator = std::fs::read_to_string(dir.join("operator-token")).unwrap();
    let project: serde_json::Value = reqwest::blocking::Client::new()
        .post(format!("{base}/v1/projects"))
        .header("x-auth-token", operator.trim())
        .json(&serde_json::json!({"name": "public"}))
        .send()
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(
        project["ingress_base_url"],
        format!(
            "https://wheel.example/p/{}",
            project["id"].as_str().unwrap()
        )
    );

    unsafe { libc::kill(child.id() as i32, libc::SIGTERM) };
    child.wait().unwrap();
    let log = std::fs::read_to_string(&log).unwrap();
    assert!(log.contains("WARN"), "{log}");
    assert!(
        log.contains(&format!("0.0.0.0:{port}")) && log.contains("every network interface"),
        "{log}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_bad_signup_setting_refuses_to_start() {
    let dir = data_dir("badsignup");
    let out = wheeld(
        &[
            "--data-dir",
            &dir.display().to_string(),
            "--bind",
            &format!("127.0.0.1:{}", free_port()),
        ],
        &[("WHEEL_SIGNUP", "sometimes")],
    )
    .output()
    .unwrap();
    assert!(!out.status.success());
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(said.contains("WHEEL_SIGNUP"), "{said}");
    std::fs::remove_dir_all(&dir).ok();
}
