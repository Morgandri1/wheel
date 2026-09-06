//! Nodes: the canonical board object. See ARCHITECTURE.md §3.
//!
//! The JSON shape is fixed by the contract:
//! ```jsonc
//! {
//!   "id": "uuid",
//!   "name": "researcher",
//!   "type": "agent",
//!   "position": { "x": 120, "y": 340 },
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

impl NodeType {
    /// "an" before a vowel sound, "a" otherwise — so messages read as English
    /// rather than "a agent node". Only `agent` and `endpoint` need "an", but
    /// the rule is written out so a new type gets it right by construction.
    pub fn article(self) -> &'static str {
        match self.as_str().chars().next() {
            Some('a' | 'e' | 'i' | 'o' | 'u') => "an",
            _ => "a",
        }
    }
}

impl std::fmt::Display for NodeType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Board coordinates: a cell, not a measurement (ARCHITECTURE.md "Position is
/// an integer cell", operator ruling 2026-09-06).
///
/// Accepts any JSON number on the way in -- an existing client mid-drag still
/// sends floats -- but rounds to the nearest cell and clamps to `i16::MIN..=
/// i16::MAX` before it is ever stored, compared, or serialised back out. That
/// clamp-on-write is why the fields are `i16` rather than `f64`: it makes
/// "already a valid cell" a property of the type instead of something every
/// reader has to re-check.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, JsonSchema, Default)]
#[serde(deny_unknown_fields)]
pub struct Position {
    pub x: i16,
    pub y: i16,
}

impl Position {
    pub fn new(x: f64, y: f64) -> Self {
        Self {
            x: clamp_cell(x),
            y: clamp_cell(y),
        }
    }
}

fn clamp_cell(v: f64) -> i16 {
    v.round().clamp(i16::MIN as f64, i16::MAX as f64) as i16
}

impl<'de> Deserialize<'de> for Position {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Raw {
            x: f64,
            y: f64,
        }
        let raw = Raw::deserialize(deserializer)?;
        Ok(Position::new(raw.x, raw.y))
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The canonical JSON of ARCHITECTURE.md §3, byte for byte. Every other
    /// crate and the Web client are generated from this shape, so a silent
    /// change here breaks them at runtime rather than at compile time.
    #[test]
    fn a_node_serializes_to_the_contracts_canonical_shape() {
        let node = Node::new(
            Uuid::nil(),
            "researcher".parse().unwrap(),
            Position::new(120.0, 340.0),
            NodeConfig::Ctx(CtxConfig {
                markdown: "hello".into(),
            }),
        );
        let v = serde_json::to_value(&node).unwrap();

        assert_eq!(v["name"], "researcher");
        assert_eq!(v["type"], "ctx", "the tag must be a sibling of config");
        assert_eq!(v["config"]["markdown"], "hello");
        assert_eq!(v["position"]["x"], 120.0);
        assert_eq!(v["position"]["y"], 340.0);
        assert_eq!(v["wires"], serde_json::json!([]));

        // No stray keys and none missing. (Order is not asserted: serde_json's
        // Value sorts them, so an order assertion here would be testing
        // serde_json rather than the contract.)
        let mut keys: Vec<&str> = v.as_object().unwrap().keys().map(|k| k.as_str()).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["config", "id", "name", "position", "type", "wires"]);

        let back: Node = serde_json::from_value(v).unwrap();
        assert_eq!(back, node);
    }

    /// `type` is the discriminant the API and the UI switch on, so each
    /// spelling is pinned and each must agree with `node_type()`.
    #[test]
    fn every_node_type_has_one_spelling_used_everywhere() {
        let cases: Vec<(NodeConfig, NodeType, &str)> = vec![
            (
                NodeConfig::Agent(AgentConfig::default()),
                NodeType::Agent,
                "agent",
            ),
            (
                NodeConfig::Ctx(CtxConfig {
                    markdown: String::new(),
                }),
                NodeType::Ctx,
                "ctx",
            ),
            (
                NodeConfig::Table(TableConfig { columns: vec![] }),
                NodeType::Table,
                "table",
            ),
            (
                NodeConfig::Vault(VaultConfig { keys: vec![] }),
                NodeType::Vault,
                "vault",
            ),
            (NodeConfig::Chest(ChestConfig {}), NodeType::Chest, "chest"),
        ];
        for (config, want_type, want_str) in cases {
            assert_eq!(config.node_type(), want_type);
            assert_eq!(want_type.as_str(), want_str);
            assert_eq!(want_type.to_string(), want_str);
            assert_eq!(
                serde_json::to_value(&config).unwrap()["type"],
                want_str,
                "the serde tag and node_type() must not drift apart"
            );
            assert_eq!(
                serde_json::to_value(want_type).unwrap(),
                serde_json::Value::String(want_str.into())
            );
        }
    }

    #[test]
    fn as_agent_answers_only_for_agents() {
        let agent = NodeConfig::Agent(AgentConfig {
            system_prompt: "be useful".into(),
            ..Default::default()
        });
        assert_eq!(agent.as_agent().unwrap().system_prompt, "be useful");
        assert!(NodeConfig::Chest(ChestConfig {}).as_agent().is_none());
    }

    /// §3c#14: idle parking is what stops one process per agent living
    /// forever. An agent that omits the field must still park.
    #[test]
    fn an_agent_parks_after_the_default_idle_timeout_unless_it_says_otherwise() {
        let c: AgentConfig =
            serde_json::from_str(r#"{"harness":"claude","system_prompt":""}"#).unwrap();
        assert_eq!(c.idle_timeout_secs(), DEFAULT_IDLE_TIMEOUT_SECS);
        assert_eq!(DEFAULT_IDLE_TIMEOUT_SECS, 300, "the contract says 300");
        assert!(!c.run_on_startup);
        assert!(!c.ephemeral_context);
        assert!(c.budget.is_none());

        let explicit = AgentConfig {
            idle_timeout_secs: Some(30),
            ..Default::default()
        };
        assert_eq!(explicit.idle_timeout_secs(), 30);
        // 0 is "never park", NOT "park immediately", and must survive as 0.
        let never = AgentConfig {
            idle_timeout_secs: Some(0),
            ..Default::default()
        };
        assert_eq!(never.idle_timeout_secs(), 0);
    }

    /// A misspelled key must fail loudly. Silently ignoring it would leave an
    /// operator looking at a board that does not do what they configured.
    #[test]
    fn an_unknown_config_field_is_refused_rather_than_ignored() {
        assert!(serde_json::from_str::<AgentConfig>(
            r#"{"harness":"claude","system_prompt":"","ephemeral_contex":true}"#
        )
        .is_err());
        assert!(serde_json::from_str::<CtxConfig>(r#"{"markdown":"x","extra":1}"#).is_err());
    }

    #[test]
    fn a_script_has_a_default_timeout_and_a_file_per_language() {
        let s: ScriptConfig =
            serde_json::from_str(r#"{"language":"python","source":"print(1)"}"#).unwrap();
        assert_eq!(s.timeout_secs(), 60, "the contract's default");
        assert_eq!(ScriptLanguage::Python.main_file(), "main.py");
        assert_eq!(ScriptLanguage::Ts.main_file(), "main.ts");
        assert_eq!(ScriptLanguage::Js.main_file(), "main.js");
    }

    /// These strings are interpolated into CREATE TABLE.
    #[test]
    fn column_types_map_onto_sqlite_storage_classes() {
        assert_eq!(ColumnType::Text.sqlite_type(), "TEXT");
        assert_eq!(ColumnType::Integer.sqlite_type(), "INTEGER");
        assert_eq!(ColumnType::Real.sqlite_type(), "REAL");
        assert_eq!(ColumnType::Blob.sqlite_type(), "BLOB");
        // json is stored as TEXT and validated on write.
        assert_eq!(ColumnType::Json.sqlite_type(), "TEXT");
        assert_eq!(serde_json::to_value(ColumnType::Json).unwrap(), "json");
    }

    #[test]
    fn harnesses_and_methods_keep_their_wire_spellings() {
        assert_eq!(Harness::Claude.as_str(), "claude");
        assert_eq!(Harness::Codex.as_str(), "codex");
        assert_eq!(Harness::Claude.to_string(), "claude");
        assert_eq!(Harness::default(), Harness::Claude);
        for (m, s) in [
            (HttpMethod::Get, "GET"),
            (HttpMethod::Post, "POST"),
            (HttpMethod::Put, "PUT"),
            (HttpMethod::Delete, "DELETE"),
        ] {
            assert_eq!(m.as_str(), s);
        }
    }

    /// The article is used to build error sentences ("not an agent node").
    #[test]
    fn node_types_carry_the_article_that_reads_correctly() {
        assert_eq!(NodeType::Agent.article(), "an");
        assert_eq!(NodeType::Endpoint.article(), "an");
        assert_eq!(NodeType::Table.article(), "a");
        assert_eq!(NodeType::Vault.article(), "a");
    }

    /// `has_wire` is the check the engine runs on EVERY /v1/cli/* call, and
    /// the default answer must be no.
    #[test]
    fn a_node_without_the_wire_is_denied_by_default() {
        use crate::wire::{Wire, WireType};
        let target = Uuid::from_u128(7);
        let other = Uuid::from_u128(8);

        let mut node = Node::new(
            Uuid::nil(),
            "worker".parse().unwrap(),
            Position::default(),
            NodeConfig::Agent(AgentConfig::default()),
        );
        // No wires at all: nothing is permitted.
        assert!(!node.has_wire(target, WireType::Read, NodeType::Ctx));

        node.wires.push(Wire {
            to: target,
            wire_type: WireType::Write,
        });
        assert!(node.has_wire(target, WireType::Write, NodeType::Table));
        // write implies read on a keyspace you can enumerate (§3: table and
        // chest say so explicitly)...
        assert!(node.has_wire(target, WireType::Read, NodeType::Table));
        assert!(node.has_wire(target, WireType::Read, NodeType::Chest));
        // ...but NOT on a ctx node: `wheel write <ctx>` replaces the markdown
        // whole, so being able to write one is not being able to read it.
        assert!(!node.has_wire(target, WireType::Read, NodeType::Ctx));
        // ...and never for a node it was not granted on.
        assert!(!node.has_wire(other, WireType::Read, NodeType::Table));
        // A `send` wire is not a data wire, whatever the target.
        assert!(!node.has_wire(target, WireType::Send, NodeType::Table));
    }

    #[test]
    fn a_position_is_a_plain_pair_and_defaults_to_the_origin() {
        let p = Position::new(1.5, -2.5);
        // Rounds to the nearest cell, symmetric about zero: 1.5 -> 2, -2.5 -> -3.
        assert_eq!(p.x, 2);
        assert_eq!(p.y, -3);
        assert_eq!(Position::default(), Position::new(0.0, 0.0));
        assert_eq!(
            serde_json::to_value(p).unwrap(),
            serde_json::json!({"x": 2, "y": -3})
        );
    }

    #[test]
    fn a_position_clamps_out_of_range_instead_of_overflowing() {
        assert_eq!(
            Position::new(99999.0, -99999.0),
            Position::new(32767.0, -32768.0)
        );
        assert_eq!(
            Position::new(f64::MAX, f64::MIN),
            Position::new(32767.0, -32768.0)
        );
    }

    #[test]
    fn a_position_rounds_on_the_way_in_from_json() {
        let p: Position =
            serde_json::from_value(serde_json::json!({"x": 10.6, "y": -10.6})).unwrap();
        assert_eq!(p, Position::new(11.0, -11.0));
    }

    #[test]
    fn a_position_still_denies_unknown_fields() {
        let r: Result<Position, _> =
            serde_json::from_value(serde_json::json!({"x": 1, "y": 2, "z": 3}));
        assert!(r.is_err());
    }

    #[test]
    fn an_mcp_node_reports_the_transport_it_is_configured_for() {
        let stdio: McpConfig =
            serde_json::from_str(r#"{"transport":"stdio","command":"srv"}"#).unwrap();
        assert_eq!(stdio.transport(), McpTransport::Stdio);
        let http: McpConfig =
            serde_json::from_str(r#"{"transport":"http","url":"https://e.example"}"#).unwrap();
        assert_eq!(http.transport(), McpTransport::Http);
    }
}
