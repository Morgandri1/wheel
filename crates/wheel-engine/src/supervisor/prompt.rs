//! Composing an agent's system prompt: node prompt + orchestration block +
//! injected ctx (§3 "Agent preamble").
//!
//! The strings themselves live in `wheel_core::preamble` with golden tests;
//! this module's job is to gather the board state they need.

use anyhow::Result;
use rusqlite::Connection;
use wheel_core::{compose_system_prompt, Node, NodeType, PreambleInput, WireLine, WireType};

use crate::db::board;

/// Gather this agent's wires (both directions) and its injected ctx, then
/// render the prompt.
pub fn compose_prompt(conn: &Connection, node: &Node, project_name: &str) -> Result<String> {
    let mut wires: Vec<WireLine> = Vec::new();

    for w in &node.wires {
        if let Some(peer) = board::get(conn, w.to)? {
            wires.push(WireLine {
                outgoing: true,
                peer: peer.name.clone(),
                peer_type: peer.node_type(),
                wire_type: w.wire_type,
            });
        }
    }

    let mut injected: Vec<(wheel_core::NodeName, String)> = Vec::new();
    for (from, ty) in board::wires_to(conn, node.id)? {
        let Some(peer) = board::get(conn, from)? else {
            continue;
        };
        wires.push(WireLine {
            outgoing: false,
            peer: peer.name.clone(),
            peer_type: peer.node_type(),
            wire_type: ty,
        });

        // A ctx node wired `send` into this agent is injected into its prompt.
        if peer.node_type() == NodeType::Ctx && ty == WireType::Send {
            if let wheel_core::NodeConfig::Ctx(c) = &peer.config {
                injected.push((peer.name.clone(), c.markdown.clone()));
            }
        }
    }

    // Ordered by ctx NAME, not by board position or insertion order, so the
    // composed prompt is stable: moving a node on the canvas must not silently
    // reorder an agent's context and invalidate its cache.
    injected.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()));
    wires.sort_by(|a, b| {
        (!a.outgoing, a.peer.as_str(), a.wire_type.as_str()).cmp(&(
            !b.outgoing,
            b.peer.as_str(),
            b.wire_type.as_str(),
        ))
    });

    let agent_cfg = node
        .config
        .as_agent()
        .ok_or_else(|| anyhow::anyhow!("compose_prompt called on a non-agent node"))?;

    Ok(compose_system_prompt(&PreambleInput {
        agent_name: &node.name,
        project_name,
        system_prompt: &agent_cfg.system_prompt,
        wires: &wires,
        injected_ctx: &injected,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;
    use wheel_core::{AgentConfig, CtxConfig, NodeConfig, NodeName, Position};

    fn mem() -> Connection {
        crate::db::open_memory().unwrap()
    }

    fn mk(conn: &Connection, name: &str, config: NodeConfig) -> Node {
        let n = Node::new(
            Uuid::new_v4(),
            NodeName::new(name).unwrap(),
            Position::default(),
            config,
        );
        board::create(conn, &n).unwrap();
        n
    }

    fn ctx(conn: &Connection, name: &str, md: &str) -> Node {
        mk(
            conn,
            name,
            NodeConfig::Ctx(CtxConfig {
                markdown: md.into(),
            }),
        )
    }

    fn agent(conn: &Connection, name: &str, prompt: &str) -> Node {
        mk(
            conn,
            name,
            NodeConfig::Agent(AgentConfig {
                system_prompt: prompt.into(),
                ..Default::default()
            }),
        )
    }

    #[test]
    fn ctx_wired_send_is_injected_with_its_own_header() {
        let c = mem();
        let a = agent(&c, "worker", "Be terse.");
        let n = ctx(&c, "notes", "Remember the milk.");
        board::add_wire(&c, n.id, a.id, WireType::Send, None).unwrap();

        let a = board::get(&c, a.id).unwrap().unwrap();
        let got = compose_prompt(&c, &a, "demo").unwrap();

        assert!(got.starts_with("Be terse."));
        assert!(got.contains("\n\n# Context: notes\nRemember the milk."));
        // The incoming wire is described in plain language.
        assert!(got.contains("← notes"));
        assert!(got.contains("its content is injected into your context"));
    }

    /// Ordering is by ctx NAME. If it were insertion or position order, moving
    /// a node on the canvas would silently reorder an agent's context.
    #[test]
    fn injected_ctx_is_ordered_by_name_not_by_creation_order() {
        let c = mem();
        let a = agent(&c, "worker", "");
        // Created deliberately out of alphabetical order.
        let zebra = ctx(&c, "zebra", "Z");
        let alpha = ctx(&c, "alpha", "A");
        let middle = ctx(&c, "middle", "M");
        for n in [&zebra, &alpha, &middle] {
            board::add_wire(&c, n.id, a.id, WireType::Send, None).unwrap();
        }

        let a = board::get(&c, a.id).unwrap().unwrap();
        let got = compose_prompt(&c, &a, "demo").unwrap();

        let ia = got.find("# Context: alpha").unwrap();
        let im = got.find("# Context: middle").unwrap();
        let iz = got.find("# Context: zebra").unwrap();
        assert!(ia < im && im < iz, "ctx blocks must be ordered by name");
    }

    #[test]
    fn a_ctx_node_without_a_send_wire_is_not_injected() {
        let c = mem();
        let a = agent(&c, "worker", "");
        let n = ctx(&c, "notes", "secret");
        // read is the agent's own access, not an injection wire.
        board::add_wire(&c, a.id, n.id, WireType::Read, None).unwrap();

        let a = board::get(&c, a.id).unwrap().unwrap();
        let got = compose_prompt(&c, &a, "demo").unwrap();

        assert!(
            !got.contains("# Context: notes"),
            "only ctx→agent send injects; a read wire must not"
        );
        assert!(got.contains("→ notes"), "but the wire is still listed");
    }

    #[test]
    fn both_wire_directions_appear_in_the_preamble() {
        let c = mem();
        let a = agent(&c, "planner", "");
        let peer = agent(&c, "researcher", "");
        board::add_wire(&c, a.id, peer.id, WireType::Send, None).unwrap();
        board::add_wire(&c, peer.id, a.id, WireType::Send, None).unwrap();

        let a = board::get(&c, a.id).unwrap().unwrap();
        let got = compose_prompt(&c, &a, "demo").unwrap();

        assert!(got.contains("→ researcher"));
        assert!(got.contains("you can prompt it"));
        assert!(got.contains("← researcher"));
        assert!(got.contains("it can prompt you"));
    }

    #[test]
    fn the_prompt_is_stable_across_repeated_composition() {
        // The prompt is written to a file and passed by path on every start;
        // an unstable ordering would churn the file and invalidate caching.
        let c = mem();
        let a = agent(&c, "worker", "hi");
        for name in ["b-ctx", "a-ctx", "c-ctx"] {
            let n = ctx(&c, name, name);
            board::add_wire(&c, n.id, a.id, WireType::Send, None).unwrap();
        }
        let a = board::get(&c, a.id).unwrap().unwrap();
        let first = compose_prompt(&c, &a, "demo").unwrap();
        for _ in 0..5 {
            assert_eq!(compose_prompt(&c, &a, "demo").unwrap(), first);
        }
    }

    #[test]
    fn an_agent_with_no_wires_still_gets_a_usable_prompt() {
        let c = mem();
        let a = agent(&c, "lonely", "Solo.");
        let a = board::get(&c, a.id).unwrap().unwrap();
        let got = compose_prompt(&c, &a, "demo").unwrap();
        assert!(got.starts_with("Solo."));
        assert!(got.contains("(none — you are not wired to anything yet)"));
    }
}
