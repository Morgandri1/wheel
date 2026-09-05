//! Events broadcast on `GET /v1/events` (WebSocket), one JSON object per frame.
//!
//! The API proxies this socket straight through to the browser, so these shapes
//! are a public contract: Web renders node status, log lines and messages from
//! them.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{message::Message, state::NodeState, timestamp::Timestamp, wire::WireType};

/// Where a log line came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum LogStream {
    Stdout,
    Stderr,
    /// Emitted by the engine itself (spawn, exit, session clear, ...).
    Engine,
    /// The exact bytes the engine wrote to the child's stdin (§3c#10).
    ///
    /// Carried on the same stream as everything else deliberately: a separate
    /// subscription would double the reconnect and cursor logic for no gain.
    /// `stdout` is what the agent SAID; `transcript` is what it was HANDED.
    Transcript,
}

/// One line of agent output. `seq` is monotonic per agent and is the cursor
/// used by `GET /v1/agents/:id/log?since=<seq>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LogLine {
    pub node_id: Uuid,
    pub seq: u64,
    pub stream: LogStream,
    pub at: Timestamp,
    pub text: String,
}

/// A denied capability check, surfaced so the UI can show *why* an agent's
/// call failed and so red-team/QA can assert on it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WireDenial {
    pub from: Uuid,
    /// Target as the caller named it — may not resolve to a node at all.
    pub target: String,
    pub required: WireType,
    pub reason: String,
    pub at: Timestamp,
}

/// Events pushed to `/v1/events` subscribers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// An agent's runtime state changed.
    #[serde(rename = "node.state")]
    NodeState { node_id: Uuid, state: NodeState },
    /// A message was created, delivered or acked.
    #[serde(rename = "message")]
    Message { message: Message },
    /// A line of agent output.
    #[serde(rename = "log")]
    Log { line: LogLine },
    /// Nodes or wires changed; clients should refetch `GET /v1/board`.
    /// Coarse on purpose — the board is small and this avoids a second
    /// mutation protocol that could drift from the REST one.
    #[serde(rename = "board.changed")]
    BoardChanged { at: Timestamp },
    /// A capability check failed.
    #[serde(rename = "wire.denied")]
    WireDenied { denial: WireDenial },
    /// This subscriber fell behind and events were dropped.
    ///
    /// Transport-level rather than a board fact, but it travels on the same
    /// socket and so belongs in the same union: a client that types the union
    /// from the schema and meets an undeclared frame will take its default
    /// branch, and the natural default — tear down and reconnect — is exactly
    /// wrong here. The socket is healthy and merely behind. Refetch
    /// `GET /v1/board` and keep the connection.
    #[serde(rename = "lagged")]
    Lagged { hint: String },
}

/// The message accompanying [`Event::Lagged`], in one place so the engine and
/// the docs cannot drift.
pub const LAGGED_HINT: &str = "events were dropped; refetch GET /v1/board";
