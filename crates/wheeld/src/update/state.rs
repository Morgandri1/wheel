// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

//! What the updater must remember across a restart: the pending marker the new
//! binary proves itself against, the SHAs that failed, who asked, and a history
//! row per attempt.
//!
//! `state.json` is the daemon's alone. The operator's `wheeld update` runs in
//! another process, so it writes `request.json` instead and never races the
//! daemon's writes.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use uuid::Uuid;
use wheel_core::{BlockReason, Component, UpdateNotice};

pub const HISTORY_CAP: usize = 100;
pub const BREAKER_FAILURES: usize = 3;
pub const BREAKER_WINDOW_SECS: u64 = 24 * 60 * 60;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Requester {
    Operator,
    /// `auto` mode, at a quiescent point, with nobody asking.
    Auto,
    Agent {
        project: Uuid,
        node: Uuid,
        name: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", content = "reason", rename_all = "snake_case")]
pub enum Outcome {
    Updated,
    RolledBack,
    Refused(BlockReason),
    DrainTimedOut,
    BuildFailed,
    SmokeFailed,
    SwapFailed,
    /// The pending marker named a swap the running binary never received.
    Interrupted,
}

impl Outcome {
    /// Counts toward the breaker: the attempt got far enough to fail on its own
    /// merits. A refusal or a busy board is not a failure of the code.
    pub fn is_failure(&self) -> bool {
        matches!(
            self,
            Outcome::RolledBack | Outcome::BuildFailed | Outcome::SmokeFailed | Outcome::SwapFailed
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Row {
    pub at: u64,
    pub from: String,
    pub to: String,
    pub by: Requester,
    pub outcome: Outcome,
    #[serde(default)]
    pub components: Vec<Component>,
    /// Agents that were mid-turn when a drain gave up.
    #[serde(default)]
    pub busy: Vec<String>,
    /// Whether the requester has been told.
    #[serde(default)]
    pub notified: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pending {
    pub from: String,
    pub to: String,
    pub by: Requester,
    #[serde(default)]
    pub components: Vec<Component>,
    /// Boots of the new binary so far. One that reaches a second boot without
    /// proving healthy crashed on the first.
    pub attempts: u32,
    pub at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    pub by: Requester,
    pub at: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct State {
    #[serde(default)]
    pub bad: Vec<String>,
    #[serde(default)]
    pub pending: Option<Pending>,
    #[serde(default)]
    pub request: Option<Request>,
    #[serde(default)]
    pub history: Vec<Row>,
    #[serde(default)]
    pub last_attempt: Option<u64>,
    /// The operator's last `wheeld update`: failures before it no longer count.
    #[serde(default)]
    pub breaker_reset: Option<u64>,
    /// The last notice, for `wheeld update --status` in another process.
    #[serde(default)]
    pub last_notice: Option<UpdateNotice>,
    #[serde(default)]
    pub checked_at: Option<u64>,
}

impl State {
    pub fn is_bad(&self, sha: &str) -> bool {
        self.bad.iter().any(|b| b == sha)
    }

    pub fn mark_bad(&mut self, sha: &str) {
        if !self.is_bad(sha) {
            self.bad.push(sha.to_string());
        }
    }

    pub fn record(&mut self, row: Row) {
        self.history.push(row);
        let excess = self.history.len().saturating_sub(HISTORY_CAP);
        self.history.drain(..excess);
    }

    /// Three failures in a day stop `auto` and agent requests until the
    /// operator steps in.
    pub fn suspended(&self, now: u64) -> bool {
        let since = now
            .saturating_sub(BREAKER_WINDOW_SECS)
            .max(self.breaker_reset.unwrap_or(0));
        self.history
            .iter()
            .filter(|r| r.at > since && r.outcome.is_failure())
            .count()
            >= BREAKER_FAILURES
    }
}

pub struct StateStore {
    dir: PathBuf,
    state: Mutex<State>,
}

impl StateStore {
    /// `<data>/update`, private to its owner. A state file that does not parse
    /// is set aside, not trusted and not silently discarded.
    pub fn open(dir: &Path) -> Result<Self> {
        create_private_dir(dir)?;
        let path = dir.join("state.json");
        let state = match std::fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice(&bytes) {
                Ok(s) => s,
                Err(e) => {
                    let aside = dir.join(format!("state.json.corrupt-{}", now()));
                    std::fs::rename(&path, &aside).ok();
                    tracing::error!(error = %e, kept = %aside.display(), "the update state did not parse; starting empty");
                    State::default()
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => State::default(),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        Ok(Self {
            dir: dir.to_path_buf(),
            state: Mutex::new(state),
        })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn read<T>(&self, f: impl FnOnce(&State) -> T) -> T {
        f(&self.state.lock().unwrap_or_else(|e| e.into_inner()))
    }

    /// Change and persist in one step, so what is on disk is never behind what
    /// the daemon acted on.
    pub fn update<T>(&self, f: impl FnOnce(&mut State) -> T) -> Result<T> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let out = f(&mut state);
        write_private(
            &self.dir.join("state.json"),
            &serde_json::to_vec_pretty(&*state)?,
        )?;
        Ok(out)
    }

    /// The operator's `wheeld update`, taken exactly once.
    pub fn take_operator_request(&self) -> Option<u64> {
        let path = request_path(&self.dir);
        let raw = std::fs::read(&path).ok()?;
        std::fs::remove_file(&path).ok()?;
        let req: Request = serde_json::from_slice(&raw).ok()?;
        (req.by == Requester::Operator).then_some(req.at)
    }
}

pub fn request_path(dir: &Path) -> PathBuf {
    dir.join("request.json")
}

/// Written by `wheeld update` from outside the daemon.
pub fn write_operator_request(dir: &Path) -> Result<()> {
    create_private_dir(dir)?;
    let req = Request {
        by: Requester::Operator,
        at: now(),
    };
    write_private(&request_path(dir), &serde_json::to_vec(&req)?)
}

/// Read without the daemon, for `wheeld update --status`.
pub fn peek(dir: &Path) -> State {
    std::fs::read(dir.join("state.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn create_private_dir(dir: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
        .with_context(|| format!("securing {}", dir.display()))
}

/// Written beside, then renamed over: a crash mid-write leaves the old file.
fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let tmp = path.with_extension("tmp");
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)
        .with_context(|| format!("writing {}", tmp.display()))?;
    f.write_all(bytes)?;
    f.sync_all()?;
    std::fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn dir() -> PathBuf {
        std::env::temp_dir().join(format!("wheeld-state-{}", Uuid::new_v4()))
    }

    fn row(at: u64, outcome: Outcome) -> Row {
        Row {
            at,
            from: "a".repeat(40),
            to: "b".repeat(40),
            by: Requester::Auto,
            outcome,
            components: vec![Component::Engine],
            busy: vec![],
            notified: false,
        }
    }

    #[test]
    fn state_survives_a_restart_in_a_private_file() {
        let d = dir();
        let store = StateStore::open(&d).unwrap();
        store
            .update(|s| {
                s.mark_bad("deadbeef");
                s.mark_bad("deadbeef");
                s.request = Some(Request {
                    by: Requester::Agent {
                        project: Uuid::nil(),
                        node: Uuid::nil(),
                        name: "pm".into(),
                    },
                    at: 5,
                });
                s.record(row(6, Outcome::Refused(BlockReason::CiPending)));
            })
            .unwrap();

        let again = StateStore::open(&d).unwrap();
        assert_eq!(again.read(|s| s.clone()), store.read(|s| s.clone()));
        assert!(again.read(|s| s.is_bad("deadbeef")));
        assert_eq!(again.read(|s| s.bad.len()), 1);
        assert_eq!(peek(&d), again.read(|s| s.clone()));

        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&d), 0o700);
        assert_eq!(mode(&d.join("state.json")), 0o600);
        assert_eq!(again.dir(), d.as_path());
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn a_corrupt_state_file_is_set_aside_not_trusted() {
        let d = dir();
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("state.json"), b"{not json").unwrap();
        let store = StateStore::open(&d).unwrap();
        assert_eq!(store.read(|s| s.clone()), State::default());
        let kept = std::fs::read_dir(&d)
            .unwrap()
            .filter_map(|e| e.ok())
            .any(|e| e.file_name().to_string_lossy().starts_with("state.json.corrupt-"));
        assert!(kept, "the evidence must be kept for whoever investigates");
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn history_is_bounded_and_keeps_the_newest() {
        let mut s = State::default();
        for at in 0..(HISTORY_CAP as u64 + 10) {
            s.record(row(at, Outcome::Updated));
        }
        assert_eq!(s.history.len(), HISTORY_CAP);
        assert_eq!(s.history.last().unwrap().at, HISTORY_CAP as u64 + 9);
    }

    #[test]
    fn three_failures_in_a_day_trip_the_breaker_and_the_operator_resets_it() {
        let now = 10 * BREAKER_WINDOW_SECS;
        let mut s = State::default();
        s.record(row(now - 10, Outcome::BuildFailed));
        s.record(row(now - 9, Outcome::SmokeFailed));
        s.record(row(now - 8, Outcome::Refused(BlockReason::CiFailed)));
        s.record(row(now - 7, Outcome::DrainTimedOut));
        assert!(!s.suspended(now), "a refusal or a busy board is not a failure");
        s.record(row(now - 6, Outcome::RolledBack));
        assert!(s.suspended(now));
        assert!(!s.suspended(now + BREAKER_WINDOW_SECS), "a day later it lapses");
        s.breaker_reset = Some(now - 1);
        assert!(!s.suspended(now), "the operator's `wheeld update` clears it");
        assert!(Outcome::SwapFailed.is_failure());
        assert!(!Outcome::Interrupted.is_failure());
    }

    #[test]
    fn an_operator_request_is_taken_exactly_once_and_nothing_else_counts_as_one() {
        let d = dir();
        let store = StateStore::open(&d).unwrap();
        assert_eq!(store.take_operator_request(), None);
        write_operator_request(&d).unwrap();
        assert!(store.take_operator_request().is_some());
        assert_eq!(store.take_operator_request(), None, "taken twice");

        let forged = Request {
            by: Requester::Auto,
            at: 1,
        };
        std::fs::write(request_path(&d), serde_json::to_vec(&forged).unwrap()).unwrap();
        assert_eq!(store.take_operator_request(), None);
        std::fs::write(request_path(&d), b"garbage").unwrap();
        assert_eq!(store.take_operator_request(), None);
        assert!(!request_path(&d).exists(), "a bad request is consumed, not retried");
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn the_wire_shape_of_a_row_is_readable() {
        let v = serde_json::to_value(row(1, Outcome::Refused(BlockReason::DirtyCheckout))).unwrap();
        assert_eq!(v["outcome"]["outcome"], "refused");
        assert_eq!(v["outcome"]["reason"], "dirty_checkout");
        assert_eq!(v["by"]["kind"], "auto");
    }
}
