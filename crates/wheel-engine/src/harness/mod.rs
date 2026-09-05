//! Harness adapters: how the engine drives a coding-agent CLI.
//!
//! One trait, one implementation per CLI, so the supervisor never knows which
//! harness it is talking to and a second harness (codex, M2) or a different
//! driver (the agent-sdk bridge) can land without touching delivery logic.

use std::{ffi::OsString, path::PathBuf};

use uuid::Uuid;

pub mod claude;

/// Everything needed to spawn a child for one agent node.
#[derive(Debug, Clone)]
pub struct SpawnSpec {
    pub node_id: Uuid,
    pub node_name: String,
    pub model: Option<String>,
    /// Path to the composed system prompt. A PATH, never the text: argv is
    /// world-readable across uids and the preamble carries injected ctx.
    pub prompt_file: PathBuf,
    /// Written by the engine from wired mcp nodes, when there are any.
    pub mcp_config: Option<PathBuf>,
    /// Resume an existing harness session (idle parking, §3c#14).
    pub resume: Option<String>,
    /// Per-node config/credential directory, so two agents can be two accounts.
    pub config_dir: PathBuf,
    pub cwd: PathBuf,
}

/// What the engine understood from one line of harness stdout.
///
/// `Unknown` is an ordinary variant, not an error: the real CLIs emit event
/// types that are not in our protocol (`rate_limit_event`,
/// `system/thinking_tokens` were both observed), and a parser that treated an
/// unrecognised type as a failure would break on a CLI update.
#[derive(Debug, Clone, PartialEq)]
pub enum HarnessEvent {
    /// Session started; carries the session id everything else is checked against.
    Init { session_id: String },
    /// Assistant output to show in the log.
    Text {
        session_id: Option<String>,
        text: String,
    },
    /// The turn finished. THE turn-complete signal.
    Result {
        session_id: Option<String>,
        is_error: bool,
        text: Option<String>,
    },
    /// A recognised-but-uninteresting event, or an unparseable line. Logged
    /// verbatim, never fatal.
    Unknown { raw: String },
}

/// Why a child exited without being able to work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartupFailure {
    /// The harness has no usable credentials.
    NeedsAuth,
    /// The harness refused to run at all — misconfiguration, not auth.
    Misconfigured(String),
}

pub trait Harness: Send + Sync {
    /// The executable to spawn, resolved on PATH.
    ///
    /// On the trait rather than hardcoded at the spawn site so a codex node
    /// cannot be started by running `claude` with codex's arguments.
    fn program(&self) -> &str;

    /// argv for a child. The system prompt is passed by path, never inline.
    fn argv(&self, spec: &SpawnSpec) -> Vec<OsString>;

    /// Environment additions for the child.
    fn env(&self, spec: &SpawnSpec) -> Vec<(String, String)>;

    /// Encode one delivery as the exact bytes written to the child's stdin,
    /// newline-terminated.
    fn encode_turn(&self, envelope: &str) -> String;

    /// Interpret one line of stdout.
    fn parse_line(&self, line: &str) -> HarnessEvent;

    /// Classify a child that exited during startup.
    ///
    /// Deliberately takes stderr as well as the code: running as root and being
    /// logged out BOTH exit 1, and only stderr distinguishes them. Inferring
    /// `needs_auth` from an exit code alone would report every misconfigured
    /// container as needing auth forever.
    fn classify_startup_failure(&self, code: Option<i32>, stderr: &str) -> StartupFailure;
}
