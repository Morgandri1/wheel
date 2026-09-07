//! Realising a builder-emitted board against a project.
//!
//! The input is JSON an LLM produced. It is untrusted in the ordinary sense — it can name nodes
//! that do not exist, wire types the matrix forbids, duplicate names, or a node to itself — so
//! everything here is validation first and creation second.
//!
//! Two rules shape the whole module:
//!
//! **A refusal is never a drop.** If one wire is illegal the caller is told which wire and why, by
//! name. Silently skipping it would hand back a board that looks applied and is not what was asked
//! for, and the user would have to diff it themselves to find out.
//!
//! **Nothing is created until everything is checked.** Realising a board is N separate engine calls
//! with no transaction across them, so a refusal discovered on wire 7 of 9 would otherwise leave 6
//! wires and every node behind. Pre-validating against the same matrix the engine enforces turns
//! the expected failure — the builder emitting an illegal pair — into a refusal before anything
//! exists. It does not cover engine-side failures (name collision, per-project caps), which is why
//! the apply result reports what landed rather than promising atomicity we cannot deliver here.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use wheel_core::{wire_allowed, NodeConfig, NodeType, Position, WireType};

/// A board exactly as the builder emits it.
#[derive(Debug, Clone, Deserialize)]
pub struct EmittedBoard {
    #[serde(default)]
    pub nodes: Vec<EmittedNode>,
    #[serde(default)]
    pub wires: Vec<EmittedWire>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct EmittedNode {
    pub name: String,
    #[serde(flatten)]
    pub config: NodeConfig,
    #[serde(default)]
    pub position: Position,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct EmittedWire {
    pub from: String,
    pub to: String,
    #[serde(rename = "type")]
    pub wire_type: WireType,
}

/// Why a board was refused. Every variant names the specific thing at fault.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum Refusal {
    /// Two emitted nodes share a name, so a wire naming it is ambiguous.
    DuplicateNodeName { name: String },
    /// A wire names a node that is neither emitted nor already on the board.
    UnknownNode {
        wire_from: String,
        wire_to: String,
        missing: String,
    },
    /// A node wired to itself.
    SelfWire { node: String },
    /// The wire matrix forbids this pair. Default-DENY: anything not listed is refused.
    WireNotAllowed {
        from: String,
        from_type: NodeType,
        to: String,
        to_type: NodeType,
        wire_type: WireType,
    },
}

impl Refusal {
    /// One line a person can act on, naming the node or wire at fault.
    pub fn message(&self) -> String {
        match self {
            Refusal::DuplicateNodeName { name } => {
                format!("two nodes are both named {name:?}; names must be unique on a board")
            }
            Refusal::UnknownNode { wire_from, wire_to, missing } => format!(
                "the wire {wire_from:?} -> {wire_to:?} names {missing:?}, which is not on the board \
                 and is not being created"
            ),
            Refusal::SelfWire { node } => format!("{node:?} is wired to itself"),
            Refusal::WireNotAllowed { from, from_type, to, to_type, wire_type } => format!(
                "no {} wire is allowed from a {} to a {}: {from:?} -> {to:?}",
                wire_type.as_str(),
                from_type.as_str(),
                to_type.as_str()
            ),
        }
    }
}

/// What applying the board would do, once it is known to be legal.
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    /// Nodes that do not exist yet.
    pub create_nodes: Vec<EmittedNode>,
    /// Nodes that exist and whose config the board changes. Sent as a MERGE patch, so fields the
    /// board does not mention keep their current values (RFC 7386, enforced engine-side).
    pub patch_nodes: Vec<EmittedNode>,
    /// Wires the board adds. Wires that already exist are not re-created.
    pub create_wires: Vec<EmittedWire>,
}

impl Plan {
    pub fn is_empty(&self) -> bool {
        self.create_nodes.is_empty() && self.patch_nodes.is_empty() && self.create_wires.is_empty()
    }
}

/// What is already on the board, as the apply step needs to see it.
#[derive(Debug, Clone, Default)]
pub struct ExistingBoard {
    /// name -> node type, for wire validation and to tell a create from a patch.
    pub nodes: HashMap<String, NodeType>,
    /// Wires already present, so applying the same board twice adds nothing.
    pub wires: Vec<(String, String, WireType)>,
}

/// Check an emitted board against the matrix and the current board.
///
/// Returns EVERY refusal, not the first: an LLM that got one wire wrong usually got several, and
/// handing them back one per round trip wastes the user's time.
pub fn validate(board: &EmittedBoard, existing: &ExistingBoard) -> Result<Plan, Vec<Refusal>> {
    let mut refusals = Vec::new();

    // Types by name: emitted nodes first, then whatever is already on the board.
    let mut types: HashMap<&str, NodeType> = HashMap::new();
    for node in &board.nodes {
        if types
            .insert(node.name.as_str(), node.config.node_type())
            .is_some()
        {
            refusals.push(Refusal::DuplicateNodeName {
                name: node.name.clone(),
            });
        }
    }
    for (name, ty) in &existing.nodes {
        types.entry(name.as_str()).or_insert(*ty);
    }

    for wire in &board.wires {
        if wire.from == wire.to {
            refusals.push(Refusal::SelfWire {
                node: wire.from.clone(),
            });
            continue;
        }
        let (from_type, to_type) =
            match (types.get(wire.from.as_str()), types.get(wire.to.as_str())) {
                (Some(f), Some(t)) => (*f, *t),
                (from, _) => {
                    let missing = if from.is_none() { &wire.from } else { &wire.to };
                    refusals.push(Refusal::UnknownNode {
                        wire_from: wire.from.clone(),
                        wire_to: wire.to.clone(),
                        missing: missing.clone(),
                    });
                    continue;
                }
            };
        if !wire_allowed(from_type, to_type, wire.wire_type) {
            refusals.push(Refusal::WireNotAllowed {
                from: wire.from.clone(),
                from_type,
                to: wire.to.clone(),
                to_type,
                wire_type: wire.wire_type,
            });
        }
    }

    if !refusals.is_empty() {
        return Err(refusals);
    }

    let mut create_nodes = Vec::new();
    let mut patch_nodes = Vec::new();
    for node in &board.nodes {
        if existing.nodes.contains_key(&node.name) {
            patch_nodes.push(node.clone());
        } else {
            create_nodes.push(node.clone());
        }
    }

    let create_wires = board
        .wires
        .iter()
        .filter(|w| {
            !existing
                .wires
                .iter()
                .any(|(f, t, ty)| f == &w.from && t == &w.to && *ty == w.wire_type)
        })
        .cloned()
        .collect();

    Ok(Plan {
        create_nodes,
        patch_nodes,
        create_wires,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Built from JSON on purpose: this is how a builder-emitted board actually arrives, so the
    /// tests exercise the deserialization too rather than hand-constructing types the LLM cannot.
    fn board(v: serde_json::Value) -> EmittedBoard {
        serde_json::from_value(v).expect("board parses")
    }

    fn agent(name: &str) -> serde_json::Value {
        serde_json::json!({"name": name, "type": "agent",
            "config": {"harness": "claude", "system_prompt": "hi"}})
    }
    fn ctx(name: &str) -> serde_json::Value {
        serde_json::json!({"name": name, "type": "ctx", "config": {"markdown": "notes"}})
    }
    fn vault(name: &str) -> serde_json::Value {
        serde_json::json!({"name": name, "type": "vault", "config": {"keys": []}})
    }

    #[test]
    fn a_legal_board_plans_every_node_and_wire() {
        let b = board(serde_json::json!({
            "nodes": [agent("researcher"), ctx("notes")],
            "wires": [{"from": "notes", "to": "researcher", "type": "send"}],
        }));
        let plan = validate(&b, &ExistingBoard::default()).expect("a legal board");
        assert_eq!(plan.create_nodes.len(), 2);
        assert_eq!(plan.create_wires.len(), 1);
        assert!(plan.patch_nodes.is_empty());
    }

    /// The load-bearing one. A refusal must NAME the wire; dropping it would hand back a board that
    /// looks applied and quietly is not what was asked for.
    #[test]
    fn an_illegal_wire_is_refused_by_name_and_nothing_is_planned() {
        let b = board(serde_json::json!({
            "nodes": [agent("researcher"), vault("secrets")],
            // agent -> vault WRITE is not in the matrix; only read is.
            "wires": [{"from": "researcher", "to": "secrets", "type": "write"}],
        }));
        let refusals = validate(&b, &ExistingBoard::default()).expect_err("must be refused");
        assert_eq!(refusals.len(), 1);
        let msg = refusals[0].message();
        assert!(msg.contains("researcher"), "{msg}");
        assert!(msg.contains("secrets"), "{msg}");
        assert!(msg.contains("write"), "{msg}");
        assert!(
            matches!(refusals[0], Refusal::WireNotAllowed { .. }),
            "{:?}",
            refusals[0]
        );
    }

    /// One bad wire usually means several. Returning the first would cost a round trip each.
    #[test]
    fn every_refusal_is_reported_not_only_the_first() {
        let b = board(serde_json::json!({
            "nodes": [agent("a"), vault("v")],
            "wires": [
                {"from": "a", "to": "v", "type": "write"},
                {"from": "a", "to": "ghost", "type": "send"},
                {"from": "a", "to": "a", "type": "send"},
            ],
        }));
        let refusals = validate(&b, &ExistingBoard::default()).expect_err("three problems");
        assert_eq!(refusals.len(), 3, "{refusals:?}");
    }

    #[test]
    fn a_wire_to_a_node_that_is_neither_emitted_nor_present_is_refused_naming_it() {
        let b = board(serde_json::json!({
            "nodes": [agent("a")],
            "wires": [{"from": "a", "to": "nowhere", "type": "send"}],
        }));
        let r = validate(&b, &ExistingBoard::default()).expect_err("unknown node");
        assert!(r[0].message().contains("nowhere"), "{}", r[0].message());
    }

    #[test]
    fn a_node_wired_to_itself_is_refused() {
        let b = board(serde_json::json!({
            "nodes": [agent("a")],
            "wires": [{"from": "a", "to": "a", "type": "send"}],
        }));
        let r = validate(&b, &ExistingBoard::default()).expect_err("self wire");
        assert!(matches!(r[0], Refusal::SelfWire { .. }), "{:?}", r[0]);
    }

    #[test]
    fn two_nodes_with_one_name_are_refused_because_a_wire_naming_it_is_ambiguous() {
        let b = board(serde_json::json!({
            "nodes": [agent("twin"), ctx("twin")],
            "wires": [],
        }));
        let r = validate(&b, &ExistingBoard::default()).expect_err("duplicate");
        assert!(
            matches!(r[0], Refusal::DuplicateNodeName { .. }),
            "{:?}",
            r[0]
        );
    }

    /// A wire may name a node that already exists and is not being re-emitted.
    #[test]
    fn a_wire_may_reference_a_node_already_on_the_board() {
        let mut existing = ExistingBoard::default();
        existing.nodes.insert("researcher".into(), NodeType::Agent);
        let b = board(serde_json::json!({
            "nodes": [ctx("notes")],
            "wires": [{"from": "notes", "to": "researcher", "type": "send"}],
        }));
        let plan = validate(&b, &existing).expect("legal against the current board");
        assert_eq!(plan.create_nodes.len(), 1);
        assert_eq!(plan.create_wires.len(), 1);
    }

    /// PM's third: improve applies only the delta. An existing node becomes a PATCH — a merge, so
    /// the fields the board does not mention keep their values — and an untouched node is absent
    /// from the plan entirely.
    #[test]
    fn improve_plans_only_the_delta_and_leaves_untouched_nodes_alone() {
        let mut existing = ExistingBoard::default();
        existing.nodes.insert("researcher".into(), NodeType::Agent);
        existing.nodes.insert("untouched".into(), NodeType::Ctx);

        let b = board(serde_json::json!({
            "nodes": [agent("researcher"), ctx("new-notes")],
            "wires": [{"from": "new-notes", "to": "researcher", "type": "send"}],
        }));
        let plan = validate(&b, &existing).expect("legal");

        assert_eq!(plan.patch_nodes.len(), 1);
        assert_eq!(plan.patch_nodes[0].name, "researcher");
        assert_eq!(plan.create_nodes.len(), 1);
        assert_eq!(plan.create_nodes[0].name, "new-notes");
        assert!(
            !plan
                .create_nodes
                .iter()
                .chain(&plan.patch_nodes)
                .any(|n| n.name == "untouched"),
            "a node the board never mentioned appeared in the plan"
        );
    }

    /// Applying the same board twice must not stack duplicate wires.
    #[test]
    fn a_wire_that_already_exists_is_not_created_again() {
        let mut existing = ExistingBoard::default();
        existing.nodes.insert("notes".into(), NodeType::Ctx);
        existing.nodes.insert("researcher".into(), NodeType::Agent);
        existing
            .wires
            .push(("notes".into(), "researcher".into(), WireType::Send));

        let b = board(serde_json::json!({
            "nodes": [],
            "wires": [{"from": "notes", "to": "researcher", "type": "send"}],
        }));
        let plan = validate(&b, &existing).expect("legal");
        assert!(plan.create_wires.is_empty(), "{:?}", plan.create_wires);
        assert!(
            plan.is_empty(),
            "re-applying an unchanged board should plan nothing"
        );
    }

    /// An empty board is legal and plans nothing — it must not be an error.
    #[test]
    fn an_empty_board_is_legal_and_plans_nothing() {
        let b = board(serde_json::json!({"nodes": [], "wires": []}));
        assert!(validate(&b, &ExistingBoard::default())
            .expect("legal")
            .is_empty());
    }
}
