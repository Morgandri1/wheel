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
    /// Process stopped after `idle_timeout_secs` to save compute (§3c#14). The
    /// session id is retained and the next message resumes it transparently, so
    /// parking never loses context.
    Parked,
    /// Stopped because the agent's `budget` was reached. Requires operator
    /// action; the engine will not restart it on its own.
    BudgetExhausted,
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
            AgentStatus::Parked => "parked",
            AgentStatus::BudgetExhausted => "budget_exhausted",
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Default)]
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
    /// Where this agent's process lives: `"cloud"`, a local runner id, or
    /// `None` for **unhosted** — a first-class alarming state, not an absence
    /// (§3e). An agent nobody can run is a broken agent and the UI says so.
    #[serde(default)]
    pub hosted_on: Option<String>,
    /// Observed spend, from the harness's usage events. Drives `budget`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spend: Option<Spend>,
}

/// Accumulated cost for an agent's current lifetime.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema, Default)]
pub struct Spend {
    #[serde(default)]
    pub turns: u64,
    #[serde(default)]
    pub usd: f64,
}

/// `state` as reported next to a node on `GET /v1/board`. Only agent nodes
/// currently carry state; the enum leaves room for others (e.g. table row
/// counts) without a breaking change.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum NodeState {
    Agent(AgentState),
}

/// A node plus its observed state: `GET /v1/board` returns `{ ...node, state }`.
///
/// `state` is always present and is **null for non-agent node types** — not
/// omitted.
#[derive(Debug, Clone, PartialEq, Deserialize, JsonSchema)]
pub struct NodeWithState {
    #[serde(flatten)]
    pub node: crate::node::Node,
    #[serde(default)]
    pub state: Option<NodeState>,
}

/// Serialized by hand: serde's `flatten` silently DROPS an `Option::None` field
/// instead of writing null, so a ctx node's board entry would come back with no
/// `state` key at all. Web must be able to tell "this node has no state" from
/// "the board hasn't loaded yet", so the key is always emitted.
/// Deserialization keeps the derive — a missing key defaults to `None`.
impl Serialize for NodeWithState {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::Error;
        let mut v = serde_json::to_value(&self.node).map_err(S::Error::custom)?;
        let obj = v
            .as_object_mut()
            .ok_or_else(|| S::Error::custom("a node must serialize to a JSON object"))?;
        obj.insert(
            "state".into(),
            serde_json::to_value(&self.state).map_err(S::Error::custom)?,
        );
        v.serialize(s)
    }
}

/// Whether an agent's harness currently holds usable credentials
/// (`GET /v1/agents/:id/auth`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AuthStatus {
    /// Whether credentials are STORED. Not whether they work: only the
    /// harness's own probe can say that, and claiming otherwise would tell an
    /// operator they are fine right up until the first request fails.
    pub authenticated: bool,
    /// Which kind of credential is stored, or `null` when there is none.
    pub mode: Option<CredentialKind>,
    /// For `mode: "env"`, the name of the vault node supplying it. Never the
    /// value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Display-only account identifier (e.g. an email). Never a token.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
}

/// What kind of credential an agent node holds.
///
/// Distinct from [`AuthMode`], which is how a credential is *obtained*. The
/// difference matters because the kind decides which environment variable
/// carries it to the child, and the two Anthropic credentials are not
/// interchangeable in that envelope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CredentialKind {
    /// A provider API key: `ANTHROPIC_API_KEY` / `CODEX_API_KEY`.
    ApiKey,
    /// A long-lived OAuth token from `claude setup-token`, used by
    /// subscription accounts that have no API key: `CLAUDE_CODE_OAUTH_TOKEN`.
    OauthToken,
    /// The harness's own login credentials on disk, written by its OAuth flow.
    /// Carried by the node's config dir, not by an environment variable.
    OauthSession,
    /// Supplied by a wired `vault` node and exported into the child's
    /// environment at spawn. This is how one project runs several accounts of
    /// the same provider: one vault per account, and an agent uses the vault
    /// it has a read wire to.
    Env,
}

impl CredentialKind {
    pub fn as_str(self) -> &'static str {
        match self {
            CredentialKind::ApiKey => "api_key",
            CredentialKind::OauthToken => "oauth_token",
            CredentialKind::OauthSession => "oauth_session",
            CredentialKind::Env => "env",
        }
    }
}

/// Environment variables that authenticate a harness.
///
/// A vault key with one of these names is a credential, which is why two
/// wired vaults defining the same one is refused rather than resolved: the
/// engine would have to pick an account on the user's behalf, and whichever
/// it picked would be right half the time and silent about it.
pub const CREDENTIAL_KEYS: [&str; 3] = [
    "CLAUDE_CODE_OAUTH_TOKEN",
    "ANTHROPIC_API_KEY",
    "CODEX_API_KEY",
];

pub fn is_credential_key(key: &str) -> bool {
    CREDENTIAL_KEYS.contains(&key)
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
