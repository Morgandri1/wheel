//! Nodes: the canonical board object. See ARCHITECTURE.md §3.
//!
//! The JSON shape is fixed by the contract:
//! ```jsonc
//! {
//!   "id": "uuid",
//!   "name": "researcher",
//!   "type": "agent",
//!   "position": { "x": 120.0, "y": 340.0 },
//!   "wires": [ { "to": "<node id>", "type": "read" } ],
//!   "config": { ... }
//! }
//! ```
//! `type` and `config` are produced by an adjacently-tagged enum
//! ([`NodeConfig`]) flattened into [`Node`], so the tag and the payload cannot
//! drift apart: it is impossible to construct a `Node` whose `type` says
//! `agent` but whose `config` is a ctx config.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    name::{Ident, NodeName},
    tool::ToolConfig,
    wire::Wire,
};

/// The eight node types.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum NodeType {
    Agent,
    Ctx,
    Table,
    Endpoint,
    Script,
    Mcp,
    Vault,
    Chest,
    Tool,
}

impl NodeType {
    pub const ALL: [NodeType; 9] = [
        NodeType::Agent,
        NodeType::Ctx,
        NodeType::Table,
        NodeType::Endpoint,
        NodeType::Script,
        NodeType::Mcp,
        NodeType::Vault,
        NodeType::Chest,
        NodeType::Tool,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            NodeType::Agent => "agent",
            NodeType::Ctx => "ctx",
            NodeType::Table => "table",
            NodeType::Endpoint => "endpoint",
            NodeType::Script => "script",
            NodeType::Mcp => "mcp",
            NodeType::Vault => "vault",
            NodeType::Chest => "chest",
            NodeType::Tool => "tool",
        }
    }
}

impl std::fmt::Display for NodeType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Board coordinates. Floats because the canvas pans/zooms continuously.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(deny_unknown_fields)]
pub struct Position {
    pub x: f64,
    pub y: f64,
}

impl Position {
    pub fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }
}

// ---------------------------------------------------------------------------
// per-type configs
// ---------------------------------------------------------------------------

/// Which CLI backs an agent node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "lowercase")]
pub enum Harness {
    #[default]
    Claude,
    Codex,
}

impl Harness {
    pub fn as_str(self) -> &'static str {
        match self {
            Harness::Claude => "claude",
            Harness::Codex => "codex",
        }
    }
}

impl std::fmt::Display for Harness {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(deny_unknown_fields)]
pub struct AgentConfig {
    pub harness: Harness,
    /// Harness-specific model id. `None` = the CLI's own default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Appended to the harness's own system prompt, then followed by the
    /// markdown of every `ctx` node wired `send` into this agent.
    pub system_prompt: String,
    /// Start this agent when the container starts.
    #[serde(default)]
    pub run_on_startup: bool,
    /// Clear the session after every completed turn, re-applying the system
    /// prompt and ctx injections, before draining the next queued message.
    #[serde(default)]
    pub ephemeral_context: bool,
    /// Stop the process after this long idle and resume the session on the next
    /// message (§3c#14 idle parking). `None` uses
    /// [`DEFAULT_IDLE_TIMEOUT_SECS`]; `Some(0)` disables parking.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_timeout_secs: Option<u32>,
    /// Spend ceiling. On reach, the engine stops the agent with
    /// `status: budget_exhausted`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget: Option<Budget>,
}

/// Per-agent spend ceiling (§3e). Either field may be set independently.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(deny_unknown_fields)]
pub struct Budget {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_turns: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_usd: Option<f64>,
}

impl AgentConfig {
    pub fn idle_timeout_secs(&self) -> u32 {
        self.idle_timeout_secs.unwrap_or(DEFAULT_IDLE_TIMEOUT_SECS)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CtxConfig {
    pub markdown: String,
}

/// Column type of a `table` node, mapped onto a sqlite storage class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ColumnType {
    Text,
    Integer,
    Real,
    Blob,
    /// Stored as TEXT; the engine validates it parses as JSON on write.
    Json,
}

impl ColumnType {
    /// The sqlite column type used in `CREATE TABLE`.
    pub fn sqlite_type(self) -> &'static str {
        match self {
            ColumnType::Text | ColumnType::Json => "TEXT",
            ColumnType::Integer => "INTEGER",
            ColumnType::Real => "REAL",
            ColumnType::Blob => "BLOB",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Column {
    /// Validated with a sqlite-safe charset so it is safe to quote into DDL.
    /// Unlike a node name this may be `user`, `system`, ... — the node
    /// reserved-name list is about message addressing, not columns.
    pub name: Ident,
    #[serde(rename = "type")]
    pub column_type: ColumnType,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TableConfig {
    pub columns: Vec<Column>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "UPPERCASE")]
pub enum HttpMethod {
    Get,
    Post,
    Put,
    Delete,
}

impl HttpMethod {
    pub fn as_str(self) -> &'static str {
        match self {
            HttpMethod::Get => "GET",
            HttpMethod::Post => "POST",
            HttpMethod::Put => "PUT",
            HttpMethod::Delete => "DELETE",
        }
    }
}

/// What an endpoint returns to the HTTP caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ResponseMode {
    /// `202 {"queued": true}` as soon as the request is fanned out.
    Ack,
    /// Run the wired script synchronously and return its stdout as the body.
    Script,
}

/// How an endpoint authenticates inbound public requests (§3, M2).
///
/// Internally tagged so the two shapes are structurally distinct: `bearer`
/// cannot exist without a `vault_ref`, and `none` cannot carry one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(tag = "mode", rename_all = "lowercase", deny_unknown_fields)]
pub enum EndpointAuth {
    /// Public. Anyone who can reach the ingress URL can call it.
    #[default]
    None,
    /// Requires a bearer token matching the secret at `vault_ref`. Needs an
    /// `endpoint → vault (read)` wire; a mismatch is a 401 with no body.
    Bearer { vault_ref: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EndpointConfig {
    pub method: HttpMethod,
    /// Leading slash, no `..`. Validated by [`crate::validate::validate_endpoint_path`],
    /// and constrained in the exported schema so the static gate catches it too.
    #[schemars(regex(pattern = r"^(?!.*(?:^|/)\.\.(?:/|$))/[^\s?#]*$"))]
    pub path: String,
    pub response_mode: ResponseMode,
    #[serde(default)]
    pub auth: EndpointAuth,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ScriptLanguage {
    Python,
    Ts,
    Js,
}

impl ScriptLanguage {
    /// Filename the engine writes under `/data/scripts/<node_id>/`.
    pub fn main_file(self) -> &'static str {
        match self {
            ScriptLanguage::Python => "main.py",
            ScriptLanguage::Ts => "main.ts",
            ScriptLanguage::Js => "main.js",
        }
    }
}

pub const DEFAULT_SCRIPT_TIMEOUT_SECS: u32 = 60;

/// Idle parking default (§3c#14).
pub const DEFAULT_IDLE_TIMEOUT_SECS: u32 = 300;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ScriptConfig {
    pub language: ScriptLanguage,
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1, max = 300))]
    pub timeout_secs: Option<u32>,
}

impl ScriptConfig {
    pub fn timeout_secs(&self) -> u32 {
        self.timeout_secs.unwrap_or(DEFAULT_SCRIPT_TIMEOUT_SECS)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "lowercase")]
pub enum McpTransport {
    #[default]
    Stdio,
    Http,
}

/// MCP server config, tagged by transport.
///
/// Modelled as an enum rather than a struct of optionals so that "stdio
/// requires command", "http requires url" and "never both" are *structural* —
/// they hold in the exported JSON Schema and in the Rust type, not only in a
/// runtime check the API might forget to call. The JSON shape is unchanged:
/// `{"transport":"stdio","command":...}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "transport", rename_all = "lowercase", deny_unknown_fields)]
pub enum McpConfig {
    Stdio {
        command: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        args: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        env: Option<BTreeMap<String, String>>,
    },
    Http {
        url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        env: Option<BTreeMap<String, String>>,
    },
}

impl Default for McpConfig {
    fn default() -> Self {
        McpConfig::Stdio {
            command: String::new(),
            args: None,
            env: None,
        }
    }
}

impl McpConfig {
    pub fn transport(&self) -> McpTransport {
        match self {
            McpConfig::Stdio { .. } => McpTransport::Stdio,
            McpConfig::Http { .. } => McpTransport::Http,
        }
    }
}

/// Vault config carries only the *key names*. Values are write-only through
/// `PUT /v1/vault/:id/:key`, stored encrypted, and never returned by
/// `GET /v1/board`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(deny_unknown_fields)]
pub struct VaultConfig {
    pub keys: Vec<String>,
}

/// Chest has no configuration; its content lives on disk under
/// `/data/chest/<node_id>/` and is indexed in sqlite.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(deny_unknown_fields)]
pub struct ChestConfig {}

/// Per-type configuration, adjacently tagged so that it serializes as the
/// contract's `"type": <t>, "config": {...}` pair.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", content = "config", rename_all = "lowercase")]
pub enum NodeConfig {
    Agent(AgentConfig),
    Ctx(CtxConfig),
    Table(TableConfig),
    Endpoint(EndpointConfig),
    Script(ScriptConfig),
    Mcp(McpConfig),
    Vault(VaultConfig),
    Chest(ChestConfig),
    Tool(ToolConfig),
}

impl NodeConfig {
    pub fn node_type(&self) -> NodeType {
        match self {
            NodeConfig::Agent(_) => NodeType::Agent,
            NodeConfig::Ctx(_) => NodeType::Ctx,
            NodeConfig::Table(_) => NodeType::Table,
            NodeConfig::Endpoint(_) => NodeType::Endpoint,
            NodeConfig::Script(_) => NodeType::Script,
            NodeConfig::Mcp(_) => NodeType::Mcp,
            NodeConfig::Vault(_) => NodeType::Vault,
            NodeConfig::Chest(_) => NodeType::Chest,
            NodeConfig::Tool(_) => NodeType::Tool,
        }
    }

    pub fn as_agent(&self) -> Option<&AgentConfig> {
        match self {
            NodeConfig::Agent(c) => Some(c),
            _ => None,
        }
    }
}

/// A board node.
///
/// `type` and `config` come from the flattened [`NodeConfig`]; [`Node::node_type`]
/// reads the tag back.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Node {
    pub id: Uuid,
    pub name: NodeName,
    pub position: Position,
    /// OUTGOING wires only.
    #[serde(default)]
    pub wires: Vec<Wire>,
    #[serde(flatten)]
    pub config: NodeConfig,
}

impl Node {
    pub fn new(id: Uuid, name: NodeName, position: Position, config: NodeConfig) -> Self {
        Self {
            id,
            name,
            position,
            wires: Vec::new(),
            config,
        }
    }

    pub fn node_type(&self) -> NodeType {
        self.config.node_type()
    }

    /// Does this node have a wire to `target` that satisfies `required`?
    /// This is the check the engine performs on every `/v1/cli/*` call.
    pub fn has_wire(
        &self,
        target: Uuid,
        required: crate::wire::WireType,
        target_type: NodeType,
    ) -> bool {
        self.wires
            .iter()
            .any(|w| w.to == target && w.wire_type.satisfies(required, target_type))
    }
}
