// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

//! The update notice reaches an agent on stderr, never on stdout, and `--json`
//! carries it as a field instead (docs/proposals/auto-update.md).
//!
//! Against the real binary: which stream a line lands on is a property of the
//! process, and no function inside it can prove where its output went.

use std::io::{Read, Write};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::process::{Command, Output};

const NOTICE: &str = r#"{"state":"available","running":"abc1234","target":"def5678","components":["engine","cli"],"commits":7}"#;
const LINE: &str =
    "wheel: update available abc1234→def5678 (engine, cli; 7 commits) — run `wheel update` at a safe point";

struct Fixture {
    dir: PathBuf,
    sock: PathBuf,
    token: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.dir).ok();
    }
}

fn fixture(name: &str) -> Fixture {
    let dir = std::env::temp_dir().join(format!("wcn-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let token = dir.join("token");
    std::fs::write(&token, "t").unwrap();
    Fixture {
        sock: dir.join("e.sock"),
        dir,
        token,
    }
}

/// One canned answer on the engine socket, then the wheel command against it.
fn run(fx: &Fixture, response: String, args: &[&str]) -> Output {
    let _ = std::fs::remove_file(&fx.sock);
    let listener = UnixListener::bind(&fx.sock).unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut buf = [0u8; 4096];
        let _ = stream.read(&mut buf);
        stream.write_all(response.as_bytes()).unwrap();
    });
    let out = Command::new(env!("CARGO_BIN_EXE_wheel"))
        .args(args)
        .env(
            wheel_core::spawn::ENV_ENGINE_URL,
            format!("unix://{}", fx.sock.display()),
        )
        .env(wheel_core::spawn::ENV_TOKEN_FILE, &fx.token)
        .env_remove(wheel_core::spawn::ENV_TOKEN)
        .output()
        .expect("wheel runs");
    server.join().unwrap();
    out
}

fn answer(status: &str, header: Option<&str>, body: &str) -> String {
    let header = header
        .map(|h| format!("{}: {h}\r\n", wheel_core::UPDATE_HEADER))
        .unwrap_or_default();
    format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\n{header}\r\n{body}")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).unwrap()
}

const CTX: &str = r##"{"node":"notes","type":"ctx","value":"# exact bytes\n"}"##;

#[test]
fn the_notice_is_one_stderr_line_and_stdout_is_exactly_what_was_asked_for() {
    let fx = fixture("stderr");
    let out = run(&fx, answer("200 OK", Some(NOTICE), CTX), &["read", "notes"]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        text(&out.stdout),
        "# exact bytes\n",
        "the notice leaked into stdout"
    );
    assert_eq!(text(&out.stderr).lines().collect::<Vec<_>>(), vec![LINE]);
}

#[test]
fn json_gets_a_field_and_no_stderr_line() {
    let fx = fixture("json");
    let out = run(
        &fx,
        answer("200 OK", Some(NOTICE), CTX),
        &["read", "notes", "--json"],
    );
    assert!(out.status.success(), "{out:?}");
    let body: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(body["value"], "# exact bytes\n");
    assert_eq!(body["wheel_update"]["target"], "def5678");
    assert_eq!(body["wheel_update"]["components"][1], "cli");
    assert_eq!(text(&out.stderr), "", "--json carries it as a field instead");
}

#[test]
fn no_notice_means_no_line_and_a_malformed_one_is_dropped() {
    let fx = fixture("none");
    for header in [None, Some("not a notice"), Some(r#"{"state":"available"}"#)] {
        let out = run(&fx, answer("200 OK", header, CTX), &["read", "notes"]);
        assert!(out.status.success(), "{out:?}");
        assert_eq!(text(&out.stdout), "# exact bytes\n");
        assert_eq!(text(&out.stderr), "", "{header:?}");
    }
}

#[test]
fn a_refused_command_still_carries_the_notice_once_on_stderr() {
    let fx = fixture("denied");
    let out = run(
        &fx,
        answer(
            "403 Forbidden",
            Some(NOTICE),
            r#"{"error":{"code":"wire_denied","message":"no wire from me to notes (need: read)"}}"#,
        ),
        &["read", "notes"],
    );
    assert_eq!(out.status.code(), Some(3));
    assert_eq!(text(&out.stdout), "");
    let stderr = text(&out.stderr);
    assert_eq!(stderr.matches("update available").count(), 1, "{stderr}");
}

#[test]
fn wheel_update_refuses_when_the_policy_is_off() {
    let fx = fixture("off");
    let out = run(
        &fx,
        answer(
            "403 Forbidden",
            None,
            r#"{"error":{"code":"update_disabled","message":"auto-update is off on this deployment (WHEEL_AUTO_UPDATE is unset or off); ask the operator"}}"#,
        ),
        &["update"],
    );
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert_eq!(text(&out.stdout), "");
    assert!(text(&out.stderr).contains("WHEEL_AUTO_UPDATE"));
}

/// The body already says what the request did, so the header is not repeated.
#[test]
fn wheel_update_records_a_request_and_says_so_once() {
    let fx = fixture("req");
    let requested = NOTICE.replace("available", "requested");
    let body = format!(r#"{{"requested":true,"already_requested":false,"update":{requested}}}"#);
    let out = run(&fx, answer("202 Accepted", Some(&requested), &body), &["update"]);
    assert!(out.status.success(), "{out:?}");
    let stdout = text(&out.stdout);
    assert!(stdout.starts_with("requested — "), "{stdout}");
    assert_eq!(stdout.matches("abc1234→def5678").count(), 1, "{stdout}");
    assert_eq!(text(&out.stderr), "");
}
