//! Ctrl-c must stop it.
//!
//! `wheeld` is a daemon a person runs in their own terminal, so "it ignores SIGTERM" is not a
//! detail — it is a process they have to hunt down and kill. The engines it embeds install SIGTERM
//! handlers of their own, and a handled signal stops terminating the process, so this only shows up
//! once a project is actually running: the test starts one first.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

#[test]
fn sigterm_stops_a_daemon_with_a_project_running() {
    let dir = std::env::temp_dir().join(format!("wheeld-sigterm-{}", uuid::Uuid::new_v4()));
    let port = free_port();
    let base = format!("http://127.0.0.1:{port}");

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_wheeld"));
    cmd.args([
        "--data-dir",
        &dir.display().to_string(),
        "--bind",
        &format!("127.0.0.1:{port}"),
    ])
    .env_clear()
    .env("PATH", std::env::var("PATH").unwrap_or_default())
    .env("HOME", std::env::var("HOME").unwrap_or_default())
    .stdout(Stdio::null())
    .stderr(Stdio::null());
    // `env_clear` is deliberate — this daemon must not inherit a test's DATABASE_URL or the
    // harness's own environment — but `cargo llvm-cov` proves this binary ran at all by an env var,
    // and clearing it silently made this whole subprocess (the composed `run`, `serve_api`, and the
    // SIGTERM handler this test exists to exercise) invisible to coverage. `%p` in the value is
    // filled in by the profiling runtime with the child's own pid, so parent and child never collide.
    if let Ok(profile) = std::env::var("LLVM_PROFILE_FILE") {
        cmd.env("LLVM_PROFILE_FILE", profile);
    }
    let mut child = cmd.spawn().expect("wheeld starts");

    let client = reqwest::blocking::Client::new();
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut up = false;
    while Instant::now() < deadline {
        if client
            .get(format!("{base}/healthz"))
            .send()
            .is_ok_and(|r| r.status().is_success())
        {
            up = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(up, "wheeld never served /healthz on {base}");

    let token: String = client
        .post(format!("{base}/v1/auth/signup"))
        .json(&serde_json::json!({"email": "sigterm@example.test", "password": "wheeld-test-Passw0rd!"}))
        .send()
        .expect("signup")
        .json::<serde_json::Value>()
        .expect("signup body")["token"]
        .as_str()
        .expect("a token")
        .to_string();
    let project: serde_json::Value = client
        .post(format!("{base}/v1/projects"))
        .header("x-auth-token", &token)
        .json(&serde_json::json!({"name": "sigterm"}))
        .send()
        .expect("create")
        .json()
        .expect("project body");
    assert_eq!(
        project["status"], "running",
        "no engine was running to install a handler"
    );

    unsafe { libc::kill(child.id() as i32, libc::SIGTERM) };

    let deadline = Instant::now() + Duration::from_secs(20);
    let stopped = loop {
        match child.try_wait().expect("wait") {
            Some(_) => break true,
            None if Instant::now() >= deadline => break false,
            None => std::thread::sleep(Duration::from_millis(100)),
        }
    };
    if !stopped {
        child.kill().ok();
    }
    child.wait().ok();
    std::fs::remove_dir_all(&dir).ok();
    assert!(stopped, "wheeld ignored SIGTERM for 20s");
}
