//! SIGTERM must stop the daemon, every engine in it, and every process any agent started.
//!
//! `wheeld` is a daemon a person runs in their own terminal and Docker stops with SIGTERM, so
//! "it ignores SIGTERM" is a process they have to hunt down, and "its agents outlive it" is a
//! `claude` still spending money after the thing that owned it is gone. The test therefore runs a
//! real agent on the fake harness, gives it a grandchild, and requires both to be gone.

mod common;

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
fn sigterm_stops_the_daemon_its_engines_and_every_agent_process() {
    let dir = std::env::temp_dir().join(format!("wheeld-sigterm-{}", uuid::Uuid::new_v4()));
    let fake = common::fake_claude_dir();
    let pid_file = fake.join("grandchild.pid");
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
    .env("PATH", common::path_with(&fake))
    .env("WHEEL_SIGNUP", "open")
    .env("HOME", std::env::var("HOME").unwrap_or_default())
    .stdout(Stdio::null())
    .stderr(Stdio::null());
    // `env_clear` is deliberate — this daemon must not inherit a test's DATABASE_URL or the
    // harness's own environment — but `cargo llvm-cov` proves this binary ran at all by an env var,
    // and clearing it silently made this whole subprocess invisible to coverage. `%p` in the value
    // is filled in by the profiling runtime with the child's own pid, so parent and child never
    // collide.
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
    assert_eq!(project["status"], "running", "no engine is running");
    let engine = format!(
        "{base}/v1/projects/{}/engine/v1",
        project["id"].as_str().unwrap()
    );

    let node: serde_json::Value = client
        .post(format!("{engine}/nodes"))
        .header("x-auth-token", &token)
        .json(&common::agent_node("worker"))
        .send()
        .expect("create agent")
        .json()
        .expect("node body");
    let agent = node["id"].as_str().expect("an agent id").to_string();
    let started = client
        .post(format!("{engine}/agents/{agent}/start"))
        .header("x-auth-token", &token)
        .send()
        .expect("start agent");
    assert!(started.status().is_success(), "start: {}", started.status());
    let sent = client
        .post(format!("{engine}/agents/{agent}/send"))
        .header("x-auth-token", &token)
        .json(&common::spawn_grandchild(&pid_file))
        .send()
        .expect("send");
    assert!(sent.status().is_success(), "send: {}", sent.status());

    let grandchild = common::wait_for_pid_file(&pid_file, Duration::from_secs(60));
    let agents = common::children_matching(child.id(), &fake.display().to_string());

    unsafe { libc::kill(child.id() as i32, libc::SIGTERM) };
    let deadline = Instant::now() + Duration::from_secs(30);
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

    let survivors: Vec<i32> = agents
        .iter()
        .copied()
        .chain(grandchild)
        .filter(|pid| !common::gone_within(*pid, Duration::from_secs(5)))
        .collect();
    for pid in &survivors {
        unsafe { libc::kill(*pid, libc::SIGKILL) };
    }
    std::fs::remove_dir_all(&dir).ok();
    std::fs::remove_dir_all(&fake).ok();

    assert!(stopped, "wheeld ignored SIGTERM for 30s");
    let grandchild = grandchild.expect("the agent never ran its command: no grandchild pid");
    assert_eq!(
        agents.len(),
        1,
        "expected exactly one agent process under wheeld, found {agents:?}"
    );
    assert!(
        survivors.is_empty(),
        "processes outlived wheeld: {survivors:?} (agent {agents:?}, grandchild {grandchild})"
    );
}
