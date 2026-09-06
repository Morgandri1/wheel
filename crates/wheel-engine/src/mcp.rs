//! The tool surface an agent's harness sees over MCP (§3c #1).
//!
//! The CLI exists for scripts and humans; MCP is what an LLM should be using,
//! because a tool call is structured all the way down. That is the whole point
//! of §3c #1: a body passed as argv goes through a shell first, where backticks
//! and `$(…)` are substituted before `wheel` ever sees them, and the message
//! that arrives is silently not the one that was sent.
//!
//! The list is built PER NODE and reflects the caller's wires, so an agent is
//! shown what it can actually reach rather than a menu of denials. The engine
//! still checks every call: this is a better prompt, not a security boundary.

use rusqlite::Connection;
use serde_json::{json, Value};
use wheel_core::{Node, NodeType, WireType};

use crate::caps::Caller;

/// Every tool this node should be offered.
pub fn tools_for(conn: &Connection, me: &Caller) -> Vec<Value> {
    let reachable = me.reachable(conn);
    let mut tools = builtins(&reachable);
    tools.extend(tool_node_operations(&reachable));
    tools
}

/// A node's name and what may be done with it, for a description.
fn names_of(reachable: &[(Node, WireType)], f: impl Fn(&Node, WireType) -> bool) -> Vec<String> {
    reachable
        .iter()
        .filter(|(n, w)| f(n, *w))
        .map(|(n, _)| n.name.to_string())
        .collect()
}

/// Naming what is reachable in the description is the difference between a
/// model guessing an address and knowing one.
fn addressable(label: &str, names: &[String]) -> String {
    if names.is_empty() {
        format!("{label} (you are not wired to any)")
    } else {
        format!("{label}: {}", names.join(", "))
    }
}

fn builtins(reachable: &[(Node, WireType)]) -> Vec<Value> {
    let agents = names_of(reachable, |n, w| {
        n.node_type() == NodeType::Agent && w == WireType::Send
    });
    let readable = names_of(reachable, |n, w| {
        matches!(
            n.node_type(),
            NodeType::Ctx | NodeType::Table | NodeType::Chest
        ) && matches!(w, WireType::Read | WireType::Write)
    });
    let writable = names_of(reachable, |n, w| {
        matches!(
            n.node_type(),
            NodeType::Ctx | NodeType::Table | NodeType::Chest
        ) && w == WireType::Write
    });
    let tables = names_of(reachable, |n, _| n.node_type() == NodeType::Table);
    let vaults = names_of(reachable, |n, _| n.node_type() == NodeType::Vault);
    let scripts = names_of(reachable, |n, _| n.node_type() == NodeType::Script);

    vec![
        tool(
            "msg",
            &addressable("Send a message to another agent on the board", &agents),
            json!({
                "type": "object",
                "properties": {
                    "to": {"type": "string", "description": "agent name"},
                    "body": {"type": "string", "description": "the message; sent exactly as given"},
                    "reply_to": {"type": "string", "description": "optional message id this replies to"}
                },
                "required": ["to", "body"]
            }),
        ),
        tool(
            "read",
            &addressable(
                "Read a node: ctx markdown, a table row, or a chest blob. Address a row or path as <node>/<key>",
                &readable,
            ),
            json!({
                "type": "object",
                "properties": {"addr": {"type": "string", "description": "<node> or <node>/<key>"}},
                "required": ["addr"]
            }),
        ),
        tool(
            "write",
            &addressable(
                "Write a node: replace ctx markdown, upsert a table row, or put a chest blob",
                &writable,
            ),
            json!({
                "type": "object",
                "properties": {
                    "addr": {"type": "string", "description": "<node> or <node>/<key>"},
                    "value": {"type": "string", "description": "markdown, or a JSON object for a table row"}
                },
                "required": ["addr", "value"]
            }),
        ),
        tool(
            "rm",
            &addressable("Delete a table row or chest blob", &writable),
            json!({
                "type": "object",
                "properties": {"addr": {"type": "string", "description": "<node>/<key>"}},
                "required": ["addr"]
            }),
        ),
        tool(
            "ls",
            "List the keyspaces you can reach, or the keys inside one.",
            json!({
                "type": "object",
                "properties": {
                    "node": {"type": "string", "description": "omit to list every keyspace you can reach"},
                    "prefix": {"type": "string", "description": "only keys starting with this"}
                }
            }),
        ),
        tool(
            "query",
            &addressable("Read-only SQL against ONE table node", &tables),
            json!({
                "type": "object",
                "properties": {
                    "table": {"type": "string"},
                    "sql": {"type": "string", "description": "a single SELECT; only this table is visible"}
                },
                "required": ["table", "sql"]
            }),
        ),
        tool(
            "secret_get",
            &addressable("Read one value from a vault", &vaults),
            json!({
                "type": "object",
                "properties": {"addr": {"type": "string", "description": "<vault>/<key>"}},
                "required": ["addr"]
            }),
        ),
        tool(
            "run",
            &addressable("Run a script node and get its output", &scripts),
            json!({
                "type": "object",
                "properties": {
                    "script": {"type": "string"},
                    "args": {"type": "array", "items": {"type": "string"}}
                },
                "required": ["script"]
            }),
        ),
        tool(
            "inbox",
            "Re-read messages you were sent. Without an id, lists recent ones; the envelope id is the handle.",
            json!({
                "type": "object",
                "properties": {
                    "id": {"type": "string", "description": "a message id to read again in full"},
                    "limit": {"type": "integer"}
                }
            }),
        ),
        tool(
            "whoami",
            "Your identity and your wires, in both directions.",
            json!({"type": "object", "properties": {}}),
        ),
        tool(
            "connections",
            "Your wires with plain-language meaning: what you can prompt, and whose data you can reach.",
            json!({"type": "object", "properties": {}}),
        ),
        tool(
            "ctx_clear",
            "Clear your own context. The system prompt and injected context nodes are re-applied.",
            json!({"type": "object", "properties": {}}),
        ),
    ]
}

/// Every enabled operation on a wired tool node, as `<tool>__<op>` (§3d rule 7).
///
/// The input schema carries ONLY the agent-fill fields — the same projection
/// `wheel tool ls` uses, so what the model is offered and what the engine will
/// accept are the same list.
fn tool_node_operations(reachable: &[(Node, WireType)]) -> Vec<Value> {
    let mut out = Vec::new();
    for (node, wire) in reachable {
        if node.node_type() != NodeType::Tool || !matches!(wire, WireType::Read | WireType::Write) {
            continue;
        }
        let wheel_core::NodeConfig::Tool(cfg) = &node.config else {
            continue;
        };
        for op in crate::api::tool_routes::agent_view(node.name.as_ref(), cfg) {
            let name = op["name"].as_str().unwrap_or_default().to_string();
            let summary = op["summary"].as_str().unwrap_or_default();
            let description = if summary.is_empty() {
                format!(
                    "{} {} via the {} tool",
                    op["method"].as_str().unwrap_or("CALL"),
                    op["path"].as_str().unwrap_or_default(),
                    node.name
                )
            } else {
                summary.to_string()
            };
            out.push(tool(&name, &description, op["input_schema"].clone()));
        }
    }
    out
}

fn tool(name: &str, description: &str, input_schema: Value) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": input_schema,
    })
}
