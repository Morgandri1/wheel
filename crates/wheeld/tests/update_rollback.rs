// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

//! A build that never proved healthy is put back before this daemon serves
//! anything (docs/proposals/auto-update.md, "Rollback").
//!
//! Against the real binary, because the claim is about what happens BEFORE the
//! runtime exists: the rollback runs on the way into `cli_main`, so a build that
//! would crash while starting the runtime, opening the data directory or
//! migrating the API's database still undoes itself. No function inside the
//! daemon can prove that about the daemon's own startup order.
//!
//! It also pins the exit code the native systemd unit restarts on (75), which is
//! the seam `infra/vps/systemd/wheeld.service` depends on.

use std::path::{Path, PathBuf};
use std::process::Command;

struct Box_ {
    root: PathBuf,
    data: PathBuf,
    bin: PathBuf,
}

impl Drop for Box_ {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.root).ok();
    }
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.email=t@example.test",
            "-c",
            "user.name=t",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A deployment shaped like the VPS kit's: a checkout, a bin dir holding this
/// generation and the one it replaced, and a data dir.
fn deployment() -> (Box_, String) {
    let root = std::env::temp_dir().join(format!("wheeld-rb-{}", uuid::Uuid::new_v4()));
    let (repo, bin, data, staging) = (
        root.join("src"),
        root.join("bin"),
        root.join("data"),
        root.join("staging"),
    );
    for d in [&repo, &bin, &data, &staging] {
        std::fs::create_dir_all(d).unwrap();
    }
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("Cargo.toml"), "[workspace]\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "first"]);
    let head = git(&repo, &["rev-parse", "HEAD"]);

    // The generation that was installed, and the one it replaced.
    for name in ["wheeld", "wheel"] {
        std::fs::write(bin.join(name), format!("new {name}")).unwrap();
        std::fs::write(bin.join(format!("{name}.prev")), format!("old {name}")).unwrap();
    }
    (Box_ { root, data, bin }, head)
}

/// `attempts: 1` is a build that has booted once already without ever saying it
/// was healthy — it crashed, or it was killed, on its first try.
fn pending_marker(data: &Path, to: &str, attempts: u32) {
    let dir = data.join("update");
    std::fs::create_dir_all(&dir).unwrap();
    let marker = serde_json::json!({
        "pending": {
            "from": "c".repeat(40),
            "to": to,
            "by": {"kind": "operator"},
            "components": ["engine"],
            "attempts": attempts,
            "at": 0
        }
    });
    std::fs::write(
        dir.join("state.json"),
        serde_json::to_vec_pretty(&marker).unwrap(),
    )
    .unwrap();
}

fn spawn(b: &Box_, restart: &str) -> std::process::Child {
    Command::new(env!("CARGO_BIN_EXE_wheeld"))
        .args([
            "--data-dir",
            b.data.to_str().unwrap(),
            "--bind",
            "127.0.0.1:0",
        ])
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", b.root.to_str().unwrap())
        .env("WHEEL_AUTO_UPDATE", "prompt")
        .env("WHEEL_UPDATE_REPO", b.root.join("src"))
        .env("WHEEL_UPDATE_BIN_DIR", &b.bin)
        .env("WHEEL_UPDATE_STAGING", b.root.join("staging"))
        .env("WHEEL_UPDATE_RESTART", restart)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("wheeld runs")
}

/// Bounded, because the regression this guards against is a daemon that keeps
/// SERVING after a rollback. Waiting for it to exit would hang forever, and a
/// gate that hangs tells whoever reads CI far less than one that fails.
fn exit_code_within(mut child: std::process::Child, secs: u64) -> Option<i32> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    loop {
        match child.try_wait().expect("wait") {
            Some(status) => return status.code(),
            None if std::time::Instant::now() >= deadline => {
                child.kill().ok();
                child.wait().ok();
                return None;
            }
            None => std::thread::sleep(std::time::Duration::from_millis(100)),
        }
    }
}

fn state(b: &Box_) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(b.data.join("update/state.json")).unwrap()).unwrap()
}

#[test]
fn a_build_that_never_proved_healthy_is_put_back_before_the_daemon_serves() {
    let (b, head) = deployment();
    // This process IS the build the marker installed: an unstamped cargo build
    // reads its own commit from the checkout, which is the documented fallback.
    pending_marker(&b.data, &head, 1);

    let code = exit_code_within(spawn(&b, "exit"), 60);

    assert_eq!(
        code,
        Some(75),
        "a rolled-back daemon must exit 75 for the unit to restart it, not carry on serving the \
         build it just put back"
    );
    assert_eq!(
        std::fs::read_to_string(b.bin.join("wheeld")).unwrap(),
        "old wheeld",
        "the binary that never proved healthy is still installed"
    );
    assert_eq!(
        std::fs::read_to_string(b.bin.join("wheel")).unwrap(),
        "old wheel"
    );

    let state = state(&b);
    assert!(state["pending"].is_null(), "no marker survives a rollback");
    assert_eq!(
        state["bad"][0], head,
        "and that commit is never offered again"
    );
    assert_eq!(state["history"][0]["outcome"]["outcome"], "rolled_back");
}

/// The first boot of a new build is not a rollback: it gets its chance, and the
/// attempt is counted so a second boot without a confirm is.
#[test]
fn a_first_boot_is_given_its_chance_rather_than_rolled_back() {
    let (b, head) = deployment();
    pending_marker(&b.data, &head, 0);

    // Nothing to serve against here, so the daemon is stopped once it has made
    // its boot decision; what matters is that it did not roll back.
    let mut child = spawn(&b, "exit");

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let counted = loop {
        if std::time::Instant::now() >= deadline {
            break false;
        }
        if state(&b)["pending"]["attempts"] == 1 {
            break true;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    };
    child.kill().ok();
    child.wait().ok();

    assert!(
        counted,
        "this boot was not counted, so a crash now would not be seen as one"
    );
    assert_eq!(
        std::fs::read_to_string(b.bin.join("wheeld")).unwrap(),
        "new wheeld",
        "a first boot must not roll back"
    );
}
