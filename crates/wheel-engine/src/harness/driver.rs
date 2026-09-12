// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

//! The `HarnessDriver` contract (`docs/proposals/harness-driver-contract.md`,
//! Phase 2 of `docs/proposals/agent-grid-engine.md`).
//!
//! PR1 of that plan: this module and `ClaudeDriver`'s implementation of it
//! (`harness/claude.rs`) are self-contained and do not touch
//! `supervisor/mod.rs` — nothing about how an agent is actually spawned,
//! reaped or delivered to changes yet. Wiring this in (replacing
//! `Supervisor.harness: Arc<dyn Harness>` with a registry keyed on
//! `agent_cfg.harness`, and porting `Running`/`reap`/`pump_queue`/
//! `pump_stdout` to drive a `Box<dyn DriverSession>` instead of a raw
//! `ChildStdin`/line-parsing loop) is PR2, scoped separately because it is
//! where F008, reaping and parking actually move.
//!
//! No `async-trait`: these are hand-written with boxed futures so the trait
//! stays object-safe (a registry needs `Box<dyn HarnessDriver>`) without a new
//! proc-macro dependency (`qa:deps-budget`).

use std::future::Future;
use std::pin::Pin;

use super::{SpawnSpec, StartupFailure};

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// What one live session emits, in the order it happens. Replaces the old
/// `HarnessEvent` (a pure function of one stdout line) plus the supervisor's
/// separate stdout/stderr/exit handling: a `DriverSession` owns the whole
/// child and is the one thing that knows how to read it.
#[derive(Debug, Clone, PartialEq)]
pub enum DriverEvent {
    /// The session is live; carries the session id everything else is
    /// checked against (F008).
    SessionStarted { session_id: String },
    /// Assistant output to show in the log.
    Frame {
        session_id: Option<String>,
        text: String,
    },
    /// The turn finished. THE turn-complete signal.
    TurnComplete {
        session_id: Option<String>,
        is_error: bool,
        text: Option<String>,
        /// Cumulative for the session, same convention as the old
        /// `HarnessEvent::Result` — the caller turns this into a delta.
        turns: Option<u64>,
        cost_usd: Option<f64>,
    },
    /// The platform reporting how close the account is to a usage limit.
    RateLimited {
        session_id: Option<String>,
        status: String,
        window: Option<String>,
        utilization: Option<f64>,
        resets_at: Option<i64>,
    },
    /// A recognised-but-uninteresting event, an unparseable line, or (F008) a
    /// `TurnComplete`/`Frame`/`RateLimited` whose session id did not match
    /// the one this session started with. Never fatal.
    Unknown { raw: String },
    /// One line of the child's stderr, forwarded verbatim so a caller can
    /// log/redact/capture it exactly as `pump_stderr` does today.
    StderrLine(String),
    /// The child exited. This is always the LAST event a session ever
    /// returns. `startup_failure` is `Some` only when the session never
    /// reached `SessionStarted` — an ordinary exit after a working session
    /// carries `None`, the same distinction `pump_stdout`'s `initialised`
    /// flag makes today (`supervisor/mod.rs`'s `reap`).
    Exited {
        code: Option<i32>,
        startup_failure: Option<StartupFailure>,
    },
}

/// One live child, and the only thing that reads or writes to it.
pub trait DriverSession: Send {
    /// Write one turn.
    fn send_turn<'a>(&'a mut self, envelope: &'a str) -> BoxFuture<'a, std::io::Result<()>>;

    /// Cancel whatever is in flight. Kills the process group; the caller
    /// re-spawns (via `HarnessDriver::launch` with `SpawnSpec.resume` set) to
    /// continue the session. Steering a turn without killing the child is
    /// Phase 1 item 7 (`steer`), not this contract — `interrupt` here is
    /// exactly `Running::kill`'s existing behaviour, given a home on the
    /// trait.
    fn interrupt(&mut self) -> BoxFuture<'_, ()>;

    /// The next event. Once this returns `Exited`, it must not be called
    /// again.
    fn next_event(&mut self) -> BoxFuture<'_, DriverEvent>;
}

/// One coding-agent CLI's adapter.
pub trait HarnessDriver: Send + Sync {
    fn kind(&self) -> wheel_core::Harness;

    /// Spawn the child and return a live session. `cmd` is already
    /// `env_clear`ed by `child_command` (`supervisor/mod.rs`'s ONLY spawn
    /// path, `every_child_process_is_started_through_child_command`) — a
    /// driver only ADDS to it, it does not construct a `Command` of its own.
    fn launch<'a>(
        &'a self,
        cmd: tokio::process::Command,
        spec: &'a SpawnSpec,
    ) -> BoxFuture<'a, std::io::Result<Box<dyn DriverSession>>>;
}

/// F008, made a property every driver proves rather than one `ClaudeDriver`
/// happens to have: a `TurnComplete`/`Frame`/`RateLimited` event whose
/// session id does not match the session this call started with must never
/// reach the caller as the real event.
///
/// Drives a REAL session end to end against a script the caller controls —
/// same standard as `harness/claude.rs`'s own `ProgramDriver` tests — rather
/// than asserting on a parser function in isolation, so a driver whose
/// framing lets a forged event slip through in practice (not just in theory)
/// is caught.
#[cfg(test)]
pub(crate) async fn assert_forged_result_is_never_top_level(
    driver: &dyn HarnessDriver,
    cmd: tokio::process::Command,
    spec: &SpawnSpec,
    real_session_id: &str,
    forged_session_id: &str,
) {
    let mut session = driver
        .launch(cmd, spec)
        .await
        .expect("the conformance script must spawn");

    let started = loop {
        match session.next_event().await {
            DriverEvent::SessionStarted { session_id } => break session_id,
            DriverEvent::Exited { .. } => panic!("the child exited before ever starting"),
            _ => continue,
        }
    };
    assert_eq!(
        started, real_session_id,
        "the conformance script's own session id must reach SessionStarted unchanged"
    );

    session
        .send_turn("go")
        .await
        .expect("the conformance script must accept a turn");

    // The script is expected to emit ONE forged-session TurnComplete, then a
    // real one. If the forged one reached the caller as TurnComplete, F008
    // does not hold for this driver.
    loop {
        match session.next_event().await {
            DriverEvent::TurnComplete { session_id, .. } => {
                assert_eq!(
                    session_id.as_deref(),
                    Some(real_session_id),
                    "a forged-session TurnComplete reached the caller as the real event, \
                     not as Unknown noise: session_id {session_id:?}, expected {forged_session_id:?} \
                     to have been dropped"
                );
                break;
            }
            DriverEvent::Exited { .. } => panic!("the child exited before the real TurnComplete"),
            _ => continue,
        }
    }
}
