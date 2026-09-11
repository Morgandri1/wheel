// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

//! A real agent on QA's fake harness, and a way to watch the processes it leaves behind.
//!
//! The engine runs whatever `claude` is first on PATH, so a directory holding a `claude` that is
//! `qa/harness/fake-claude` turns every agent into a deterministic child that never touches the
//! network. Its `SH_B64` directive runs a shell command as the agent, which is how a test gives
//! the agent a grandchild to lose.

#![allow(dead_code)]

use base64::Engine as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// A private directory whose `claude` is the fake harness.
pub fn fake_claude_dir() -> PathBuf {
    let fake = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../qa/harness/fake-claude");
    let fake = fake.canonicalize().expect("qa/harness/fake-claude exists");
    let dir = std::env::temp_dir().join(format!("wheeld-fake-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    std::os::unix::fs::symlink(fake, dir.join("claude")).unwrap();
    dir
}

pub fn path_with(dir: &Path) -> String {
    format!(
        "{}:{}",
        dir.display(),
        std::env::var("PATH").unwrap_or_default()
    )
}

pub fn agent_node(name: &str) -> serde_json::Value {
    serde_json::json!({
        "name": name,
        "type": "agent",
        "position": {"x": 0, "y": 0},
        "config": {
            "harness": "claude",
            "system_prompt": "shutdown probe",
            "run_on_startup": false,
            "ephemeral_context": false
        }
    })
}

/// A message that makes the agent start `sleep` in the background and write its pid to `pid_file`.
pub fn spawn_grandchild(pid_file: &Path) -> serde_json::Value {
    let cmd = format!(
        "sleep 300 </dev/null >/dev/null 2>&1 & echo $! > {}",
        pid_file.display()
    );
    let b64 = base64::engine::general_purpose::STANDARD.encode(cmd);
    serde_json::json!({ "body": format!("<<FAKE:SH_B64={b64}>>") })
}

pub fn wait_for_pid_file(pid_file: &Path, timeout: Duration) -> Option<i32> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Some(pid) = std::fs::read_to_string(pid_file)
            .ok()
            .and_then(|s| s.trim().parse().ok())
        {
            return Some(pid);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    None
}

/// Processes whose parent is `parent` and whose command line mentions `needle`.
pub fn children_matching(parent: u32, needle: &str) -> Vec<i32> {
    let out = std::process::Command::new("ps")
        .args(["-A", "-ww", "-o", "pid=", "-o", "ppid=", "-o", "command="])
        .output()
        .expect("ps runs");
    assert!(out.status.success(), "ps exited {}", out.status);
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let pid: i32 = fields.next()?.parse().ok()?;
            let ppid: u32 = fields.next()?.parse().ok()?;
            let command = fields.collect::<Vec<_>>().join(" ");
            (ppid == parent && command.contains(needle)).then_some(pid)
        })
        .collect()
}

/// Does `pid` exist at all? A zombie still does, which is the point: an unreaped child counts.
pub fn exists(pid: i32) -> bool {
    // SAFETY: signal 0 delivers nothing; it only asks whether the process exists.
    if unsafe { libc::kill(pid, 0) } == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

pub fn gone_within(pid: i32, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if !exists(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    !exists(pid)
}
