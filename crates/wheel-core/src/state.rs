//! Runtime state. Deliberately NOT part of `config`: config is what the user
//! authored and what round-trips through the API, state is what the engine
//! observes. `GET /v1/board` reports them alongside each other (§3).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::timestamp::Timestamp;

/// Lifecycle of an agent node's child process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    /// No child process. The default, and where a node lands after `stop`.
    #[default]
    Stopped,
    /// Child spawned, harness not yet ready to accept a turn.
    Starting,
    /// The harness reports it has no usable credentials. Terminal until the
    /// operator completes `/v1/agents/:id/auth/*`.
    NeedsAuth,
    /// Child is mid-turn (a message is being processed).
    Running,
    /// Child is alive and waiting for input.
    Idle,
    /// Child exited unexpectedly or the harness reported a fatal error;
    /// `last_error` carries the detail.
    Error,
}

impl AgentStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            AgentStatus::Stopped => "stopped",
            AgentStatus::Starting => "starting",
            AgentStatus::NeedsAuth => "needs_auth",
            AgentStatus::Running => "running",
            AgentStatus::Idle => "idle",
            AgentStatus::Error => "error",
        }
    }

    /// Is the child process expected to be alive (and therefore able to take a
    /// message on stdin rather than queueing it)?
    pub fn is_live(self) -> bool {
        matches!(
            self,
            AgentStatus::Starting | AgentStatus::Running | AgentStatus::Idle
        )
    }
}

impl std::fmt::Display for AgentStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Observed state of an `agent` node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
pub struct AgentState {
    pub status: AgentStatus,
    /// The harness's own session identifier for the current session. Changes
    /// on every start and on every `ephemeral_context` clear.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_activity: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// Messages persisted but not yet delivered into the child.
    #[serde(default)]
    pub queued_messages: u32,
}

/// `state` as reported next to a node on `GET /v1/board`. Only agent nodes
/// currently carry state; the enum leaves room for others (e.g. table row
/// counts) without a breaking change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum NodeState {
    Agent(AgentState),
}

/// A node plus its observed state, as returned by `GET /v1/board`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct NodeWithState {
    #[serde(flatten)]
    pub node: crate::node::Node,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<NodeState>,
}

/// Whether an agent's harness currently holds usable credentials
/// (`GET /v1/agents/:id/auth`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AuthStatus {
    pub authenticated: bool,
    /// Display-only account identifier (e.g. an email). Never a token.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
}

/// How an agent's harness can be authenticated headlessly
/// (`POST /v1/agents/:id/auth/begin`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AuthMode {
    /// User opens `url`, enters `user_code`, engine polls for completion.
    DeviceCode,
    /// User opens `url`, completes OAuth, pastes the resulting code back into
    /// `auth/complete`.
    PasteCode,
    /// User supplies a provider API key, stored in the node's credential dir.
    ApiKey,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AuthBegin {
    pub mode: AuthMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_code: Option<String>,
    /// Human-readable steps for the UI to render verbatim.
    pub instructions: String,
    /// Opaque handle tying `auth/complete` to this `auth/begin`.
    pub session: Uuid,
}
