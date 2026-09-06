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

/// `run` is deliberately absent until script nodes exist (M2). Advertising a
/// tool whose route returns 404 teaches a model that the board is unreliable,
/// and it will stop trying things that would have worked.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::board;
    use wheel_core::{AgentConfig, CtxConfig, NodeConfig, Position, VaultConfig};

    fn node_named(name: &str, config: NodeConfig) -> Node {
        Node::new(
            uuid::Uuid::new_v4(),
            name.parse().unwrap(),
            Position::default(),
            config,
        )
    }

    /// A board with one agent wired to a few things, and one node it is NOT
    /// wired to — which is the interesting half.
    fn board_with_wires() -> (Connection, Caller) {
        let conn = crate::db::open_memory().unwrap();
        let me = node_named("worker", NodeConfig::Agent(AgentConfig::default()));
        let peer = node_named("reviewer", NodeConfig::Agent(AgentConfig::default()));
        let notes = node_named(
            "notes",
            NodeConfig::Ctx(CtxConfig {
                markdown: String::new(),
            }),
        );
        let creds = node_named("creds", NodeConfig::Vault(VaultConfig { keys: vec![] }));
        let secret_ctx = node_named(
            "not_mine",
            NodeConfig::Ctx(CtxConfig {
                markdown: String::new(),
            }),
        );
        for n in [&me, &peer, &notes, &creds, &secret_ctx] {
            board::create(&conn, n).unwrap();
        }
        board::add_wire(&conn, me.id, peer.id, WireType::Send, None).unwrap();
        board::add_wire(&conn, me.id, notes.id, WireType::Write, None).unwrap();
        board::add_wire(&conn, me.id, creds.id, WireType::Read, None).unwrap();

        let caller = Caller {
            node: board::get(&conn, me.id).unwrap().unwrap(),
        };
        (conn, caller)
    }

    fn names(tools: &[Value]) -> Vec<String> {
        tools
            .iter()
            .map(|t| t["name"].as_str().unwrap_or_default().to_string())
            .collect()
    }

    fn find<'a>(tools: &'a [Value], name: &str) -> &'a Value {
        tools.iter().find(|t| t["name"] == name).unwrap()
    }

    /// §3c#1 names the built-ins. All of them are offered, because the engine
    /// checks every call anyway and a model that cannot see a tool cannot ask
    /// about it.
    #[test]
    fn every_documented_builtin_is_offered() {
        let (conn, me) = board_with_wires();
        let tools = tools_for(&conn, &me);
        let got = names(&tools);
        for expected in [
            "msg",
            "read",
            "write",
            "rm",
            "ls",
            "query",
            "secret_get",
            "inbox",
            "whoami",
            "connections",
            "ctx_clear",
        ] {
            assert!(
                got.contains(&expected.to_string()),
                "missing {expected}: {got:?}"
            );
        }
        // `run` needs script nodes (M2). Advertising a tool whose route
        // returns 404 teaches a model that the board is unreliable, and it
        // stops trying things that would have worked.
        assert!(
            !got.contains(&"run".to_string()),
            "run has no engine route yet and must not be offered"
        );
    }

    /// Every tool must be well-formed for a harness: a name, a description a
    /// model can act on, and an object schema.
    #[test]
    fn every_tool_is_shaped_the_way_a_harness_expects() {
        let (conn, me) = board_with_wires();
        for t in tools_for(&conn, &me) {
            let name = t["name"].as_str().expect("a name");
            assert!(!name.is_empty());
            let desc = t["description"].as_str().unwrap_or_default();
            assert!(
                desc.len() > 20,
                "{name} needs a usable description: {desc:?}"
            );
            assert_eq!(t["inputSchema"]["type"], "object", "{name}");
            assert!(t["inputSchema"]["properties"].is_object(), "{name}");
        }
    }

    /// Naming what is reachable is the difference between a model guessing an
    /// address and knowing one.
    #[test]
    fn descriptions_name_the_nodes_this_agent_can_actually_reach() {
        let (conn, me) = board_with_wires();
        let tools = tools_for(&conn, &me);

        let msg = find(&tools, "msg")["description"].as_str().unwrap();
        assert!(msg.contains("reviewer"), "{msg}");

        let read = find(&tools, "read")["description"].as_str().unwrap();
        assert!(read.contains("notes"), "{read}");

        let secret = find(&tools, "secret_get")["description"].as_str().unwrap();
        assert!(secret.contains("creds"), "{secret}");

        // A node this agent has NO wire to must not be advertised anywhere.
        let all = serde_json::to_string(&tools).unwrap();
        assert!(
            !all.contains("not_mine"),
            "an unreachable node was named: {all}"
        );
    }

    /// An agent wired to nothing gets an honest description rather than a
    /// dangling "Send a message to:" with nothing after it.
    #[test]
    fn an_agent_with_no_wires_is_told_so_plainly() {
        let conn = crate::db::open_memory().unwrap();
        let me = node_named("lonely", NodeConfig::Agent(AgentConfig::default()));
        board::create(&conn, &me).unwrap();
        let caller = Caller {
            node: board::get(&conn, me.id).unwrap().unwrap(),
        };

        let tools = tools_for(&conn, &caller);
        let msg = find(&tools, "msg")["description"].as_str().unwrap();
        assert!(msg.contains("not wired to any"), "{msg}");
        // The tool is still offered: the engine answers, and a model that
        // cannot see the tool cannot be told why it failed.
        assert!(names(&tools).contains(&"msg".to_string()));
    }

    /// §3d rule 7. A wired tool node contributes `<tool>__<op>`, with only the
    /// agent-fill fields in its schema.
    #[test]
    fn a_wired_tool_node_contributes_its_operations() {
        let conn = crate::db::open_memory().unwrap();
        let me = node_named("worker", NodeConfig::Agent(AgentConfig::default()));
        let tool_node = node_named(
            "petstore",
            NodeConfig::Tool(wheel_core::ToolConfig {
                kind: wheel_core::ToolKind::Http,
                source: wheel_core::ToolSource {
                    format: wheel_core::ToolFormat::Manual,
                    raw: String::new(),
                    imported_at: wheel_core::Timestamp::now(),
                },
                base_url: "https://api.example.com".into(),
                operations: vec![wheel_core::ToolOperation {
                    id: "listPets".into(),
                    method: wheel_core::ToolMethod::Get,
                    path: "/pets".into(),
                    summary: Some("List all pets".into()),
                    enabled: true,
                    params: vec![
                        wheel_core::ToolParam {
                            name: "limit".into(),
                            location: wheel_core::ParamLocation::Query,
                            required: false,
                            description: None,
                            schema: None,
                            fill: wheel_core::Fill::agent(),
                        },
                        wheel_core::ToolParam {
                            name: "Authorization".into(),
                            location: wheel_core::ParamLocation::Header,
                            required: false,
                            description: None,
                            schema: None,
                            fill: wheel_core::Fill {
                                mode: wheel_core::FillMode::Vault,
                                value: None,
                                vault_ref: Some("creds/KEY".into()),
                            },
                        },
                    ],
                }],
            }),
        );
        board::create(&conn, &me).unwrap();
        board::create(&conn, &tool_node).unwrap();
        board::add_wire(&conn, me.id, tool_node.id, WireType::Read, None).unwrap();

        let caller = Caller {
            node: board::get(&conn, me.id).unwrap().unwrap(),
        };
        let tools = tools_for(&conn, &caller);
        let op = find(&tools, "petstore__listPets");
        assert_eq!(op["description"], "List all pets");

        let props = op["inputSchema"]["properties"].as_object().unwrap();
        assert!(props.contains_key("limit"));
        // The vault-pinned header is not the agent's to fill, so it is not in
        // the schema and its ref is nowhere in the payload.
        assert!(!props.contains_key("Authorization"));
        let all = serde_json::to_string(&tools).unwrap();
        assert!(!all.contains("creds/KEY"), "a vault ref leaked: {all}");
    }
}
