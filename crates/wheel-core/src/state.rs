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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::{AgentConfig, ChestConfig, Node, NodeConfig, Position};

    fn node(config: NodeConfig) -> Node {
        Node::new(
            Uuid::nil(),
            "n".parse().unwrap(),
            Position::default(),
            config,
        )
    }

    /// Every spelling the UI switches on. `needs_auth` and `budget_exhausted`
    /// are the two that snake_case actually changes, so they matter most.
    #[test]
    fn agent_statuses_keep_their_wire_spellings() {
        for (s, want) in [
            (AgentStatus::Stopped, "stopped"),
            (AgentStatus::Starting, "starting"),
            (AgentStatus::NeedsAuth, "needs_auth"),
            (AgentStatus::Running, "running"),
            (AgentStatus::Idle, "idle"),
            (AgentStatus::Parked, "parked"),
            (AgentStatus::BudgetExhausted, "budget_exhausted"),
            (AgentStatus::Error, "error"),
        ] {
            assert_eq!(s.as_str(), want);
            assert_eq!(s.to_string(), want);
            // as_str() and serde must not drift apart: the engine writes one
            // and the UI reads the other.
            assert_eq!(serde_json::to_value(s).unwrap(), want);
            assert_eq!(
                serde_json::from_str::<AgentStatus>(&format!("\"{want}\"")).unwrap(),
                s
            );
        }
    }

    /// `is_live` decides whether a message is written to stdin now or queued.
    /// Getting `parked` wrong here would write to a process that isn't there.
    #[test]
    fn only_a_status_with_a_real_process_is_live() {
        for live in [
            AgentStatus::Starting,
            AgentStatus::Running,
            AgentStatus::Idle,
        ] {
            assert!(live.is_live(), "{live} should be live");
        }
        for dead in [
            AgentStatus::Stopped,
            AgentStatus::NeedsAuth,
            AgentStatus::Parked,
            AgentStatus::BudgetExhausted,
            AgentStatus::Error,
        ] {
            assert!(!dead.is_live(), "{dead} has no child process");
        }
    }

    #[test]
    fn an_agent_that_has_never_run_is_stopped_with_nothing_queued() {
        let s = AgentState::default();
        assert_eq!(s.status, AgentStatus::Stopped);
        assert_eq!(s.queued_messages, 0);
        assert!(s.session_id.is_none());
        assert!(s.hosted_on.is_none(), "unhosted until something hosts it");
        assert!(s.spend.is_none());
    }

    /// §3e: an agent nobody can run is a broken agent, and the UI has to be
    /// able to say so. That requires `hosted_on: null` to survive the wire
    /// rather than being dropped as an absent Option.
    #[test]
    fn an_unhosted_agent_reports_hosted_on_null_rather_than_omitting_it() {
        let v = serde_json::to_value(AgentState::default()).unwrap();
        assert!(
            v.get("hosted_on").is_some(),
            "unhosted must be visible: {v}"
        );
        assert!(v["hosted_on"].is_null());
        assert_eq!(v["queued_messages"], 0);
        // Fields that are genuinely absent stay absent.
        assert!(v.get("session_id").is_none());
        assert!(v.get("last_error").is_none());
    }

    /// The reason `Serialize` is hand-written: serde's `flatten` DROPS a None
    /// field instead of writing null, so a ctx node would come back with no
    /// `state` key and Web could not tell "no state" from "not loaded yet".
    #[test]
    fn a_node_without_state_still_carries_an_explicit_null_state() {
        let nws = NodeWithState {
            node: node(NodeConfig::Chest(ChestConfig {})),
            state: None,
        };
        let v = serde_json::to_value(&nws).unwrap();
        assert!(
            v.as_object().unwrap().contains_key("state"),
            "the state key must always be present: {v}"
        );
        assert!(v["state"].is_null());
        // ...and the node's own fields are still flattened alongside it.
        assert_eq!(v["type"], "chest");
        assert_eq!(v["name"], "n");

        let back: NodeWithState = serde_json::from_value(v).unwrap();
        assert_eq!(back, nws);
    }

    #[test]
    fn an_agents_state_is_tagged_and_round_trips() {
        let nws = NodeWithState {
            node: node(NodeConfig::Agent(AgentConfig::default())),
            state: Some(NodeState::Agent(AgentState {
                status: AgentStatus::Parked,
                session_id: Some("sess-1".into()),
                queued_messages: 2,
                hosted_on: Some("cloud".into()),
                ..Default::default()
            })),
        };
        let v = serde_json::to_value(&nws).unwrap();
        assert_eq!(v["state"]["kind"], "agent");
        assert_eq!(v["state"]["status"], "parked");
        assert_eq!(v["state"]["queued_messages"], 2);
        assert_eq!(v["state"]["hosted_on"], "cloud");
        assert_eq!(v["type"], "agent");

        let back: NodeWithState = serde_json::from_value(v).unwrap();
        assert_eq!(back, nws);
    }

    #[test]
    fn credential_kinds_keep_their_wire_spellings() {
        for (k, want) in [
            (CredentialKind::ApiKey, "api_key"),
            (CredentialKind::OauthToken, "oauth_token"),
            (CredentialKind::OauthSession, "oauth_session"),
            (CredentialKind::Env, "env"),
        ] {
            assert_eq!(k.as_str(), want);
            assert_eq!(serde_json::to_value(k).unwrap(), want);
        }
    }

    /// `mode` is a union including null, and the UI renders each arm. An
    /// unauthenticated agent must serialize `mode: null`, not omit it.
    #[test]
    fn auth_status_reports_no_credential_as_an_explicit_null_mode() {
        let none = AuthStatus {
            authenticated: false,
            mode: None,
            source: None,
            account: None,
        };
        let v = serde_json::to_value(&none).unwrap();
        assert_eq!(v["authenticated"], false);
        assert!(v.get("mode").is_some() && v["mode"].is_null(), "{v}");
        assert!(v.get("source").is_none());
        assert!(v.get("account").is_none());

        // A vault-supplied credential names the vault, never the value.
        let from_vault = AuthStatus {
            authenticated: true,
            mode: Some(CredentialKind::Env),
            source: Some("anthropic-personal".into()),
            account: None,
        };
        let v = serde_json::to_value(&from_vault).unwrap();
        assert_eq!(v["mode"], "env");
        assert_eq!(v["source"], "anthropic-personal");
    }

    /// These three names decide what counts as a credential, and therefore
    /// which vault pairs are refused as ambiguous.
    #[test]
    fn the_credential_keys_are_exactly_the_three_the_harnesses_read() {
        assert_eq!(
            CREDENTIAL_KEYS,
            [
                "CLAUDE_CODE_OAUTH_TOKEN",
                "ANTHROPIC_API_KEY",
                "CODEX_API_KEY"
            ]
        );
        for k in CREDENTIAL_KEYS {
            assert!(is_credential_key(k));
        }
        // Case-sensitive, and near-misses are ordinary secrets.
        assert!(!is_credential_key("anthropic_api_key"));
        assert!(!is_credential_key("OPENAI_API_KEY"));
        assert!(!is_credential_key("ANTHROPIC_API_KEY_2"));
        assert!(!is_credential_key(""));
    }

    /// The two auth shapes stay distinct: claude submits a code, codex polls.
    #[test]
    fn auth_begin_keeps_the_two_flows_apart() {
        assert_eq!(
            serde_json::to_value(AuthMode::PasteCode).unwrap(),
            "paste_code"
        );
        assert_eq!(
            serde_json::to_value(AuthMode::DeviceCode).unwrap(),
            "device_code"
        );
        assert_eq!(serde_json::to_value(AuthMode::ApiKey).unwrap(), "api_key");

        let begin = AuthBegin {
            mode: AuthMode::PasteCode,
            url: Some("https://claude.com/cai/oauth/authorize?x=1".into()),
            user_code: None,
            instructions: "Open the link".into(),
            session: Uuid::nil(),
        };
        let v = serde_json::to_value(&begin).unwrap();
        assert_eq!(v["mode"], "paste_code");
        assert!(
            v.get("user_code").is_none(),
            "paste-code has no user_code to show: {v}"
        );
        assert_eq!(serde_json::from_value::<AuthBegin>(v).unwrap(), begin);
    }

    #[test]
    fn spend_accumulates_from_zero() {
        let s = Spend::default();
        assert_eq!(s.turns, 0);
        assert_eq!(s.usd, 0.0);
        let v = serde_json::to_value(Spend {
            turns: 3,
            usd: 0.25,
        })
        .unwrap();
        assert_eq!(v["turns"], 3);
        assert_eq!(v["usd"], 0.25);
    }
}
