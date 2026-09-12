// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

//! Realising a builder-emitted board against a project.
//!
//! The input is JSON an LLM produced. It is untrusted in the ordinary sense — it can name nodes
//! that do not exist, wire types the matrix forbids, duplicate names, or a node to itself — so
//! everything here is validation first and creation second.
//!
//! Three rules shape the whole module:
//!
//! **A refusal is never a drop.** If one wire is illegal the caller is told which wire and why, by
//! name. Silently skipping it would hand back a board that looks applied and is not what was asked
//! for, and the user would have to diff it themselves to find out.
//!
//! **Nothing is created until everything is checked.** Realising a board is N separate engine calls
//! with no transaction across them, so a refusal discovered on wire 7 of 9 would otherwise leave 6
//! wires and every node behind. Pre-validating against the same matrix the engine enforces turns
//! the expected failure — the builder emitting an illegal pair — into a refusal before anything
//! exists. It does not cover engine-side failures — a name collision, or anything else the engine
//! decides at creation time — which is why the apply result reports what landed rather than
//! promising atomicity we cannot deliver here.
//!
//! **Omission never destroys.** A node or wire this board does not mention is left exactly as it
//! is; removing something requires naming it in `remove`. An LLM that abbreviates ("…the rest is
//! unchanged") or is talked into leaving a node out by text on the board it is reading would
//! otherwise be proposing a deletion, and it would read as an innocent updated board.
//!
//! **There is no per-project node cap at any layer today.** `MAX_NODES`/`MAX_WIRES` below bound
//! ONE REQUEST, not a project total, so a caller can still grow a board without limit an apply at
//! a time.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use uuid::Uuid;
use wheel_core::{
    validate_config_with, validate_name, validate_table_name, wire_allowed, Harness, NodeConfig,
    NodeType, Position, WireType,
};

/// A board exactly as the builder emits it.
///
/// Both shapes are accepted, because two callers write it two ways and neither is wrong: the
/// builder nests `wires` on the source node addressed by id (`BUILDER_PROMPT.md`'s contract),
/// while a template file lists them flat by name. They are normalised to one thing here — before
/// this, a nested wire was silently ignored and a builder board applied as nodes with NO wires,
/// reported as a complete success.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct EmittedBoard {
    #[serde(default)]
    pub nodes: Vec<EmittedNode>,
    #[serde(default)]
    pub wires: Vec<EmittedWire>,
    /// What to take away. Never inferred from what the board leaves out.
    #[serde(default)]
    pub remove: Removals,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Removals {
    #[serde(default)]
    pub nodes: Vec<String>,
    #[serde(default)]
    pub wires: Vec<EmittedWire>,
}

/// A node as emitted, keeping BOTH the typed config (for validation) and the raw one (for the
/// patch).
///
/// The raw copy is load-bearing. Typed deserialisation turns a field the board never wrote into
/// an explicit default — `run_on_startup: false` — and a merge patch built from that would write
/// that `false` over a stored `true`. The board's own words are the only safe thing to send.
#[derive(Debug, Clone, PartialEq)]
pub struct EmittedNode {
    pub name: String,
    pub config: NodeConfig,
    pub raw_config: serde_json::Value,
    pub position: Position,
    /// The builder gives every node an id and rewires by it; a template has none.
    pub id: Option<String>,
    /// Outgoing wires, addressed by node id.
    pub wires: Vec<NestedWire>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct NestedWire {
    pub to: String,
    #[serde(rename = "type")]
    pub wire_type: WireType,
}

impl<'de> Deserialize<'de> for EmittedNode {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(d)?;
        EmittedNode::from_value(&value).map_err(serde::de::Error::custom)
    }
}

impl EmittedNode {
    fn from_value(value: &serde_json::Value) -> Result<Self, String> {
        let object = value.as_object().ok_or("a node must be a json object")?;
        let name = object
            .get("name")
            .and_then(|n| n.as_str())
            .ok_or("a node needs a name")?
            .to_string();
        let raw_config = object
            .get("config")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({}));
        let typed = serde_json::json!({
            "type": object.get("type").cloned().unwrap_or(serde_json::Value::Null),
            "config": raw_config.clone(),
        });
        let config: NodeConfig = serde_json::from_value(typed)
            .map_err(|e| format!("{name:?} is not a node this board can hold: {e}"))?;
        let position = match object.get("position") {
            Some(p) => serde_json::from_value(p.clone())
                .map_err(|e| format!("{name:?} has an unreadable position: {e}"))?,
            None => Position::default(),
        };
        let wires = match object.get("wires") {
            Some(w) => serde_json::from_value(w.clone())
                .map_err(|e| format!("{name:?} has an unreadable wire: {e}"))?,
            None => Vec::new(),
        };
        Ok(Self {
            name,
            config,
            raw_config,
            position,
            id: object
                .get("id")
                .and_then(|i| i.as_str())
                .map(str::to_string),
            wires,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
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
    /// The board could not be read at all.
    MalformedBoard { reason: String },
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
    /// An emitted node reuses the name of an existing node of a DIFFERENT type.
    NodeTypeMismatch {
        name: String,
        existing_type: NodeType,
        emitted_type: NodeType,
    },
    /// The board keeps an existing node's id but gives it another name.
    ///
    /// Refused rather than guessed. By id it is a rename; by name it is a new node beside an
    /// orphan, and one of those two readings quietly abandons whatever the old name held.
    RenameNotSupported {
        id: String,
        existing_name: String,
        emitted_name: String,
    },
    /// The board names a node that already exists, and this apply may only create.
    PatchNotPermitted { name: String },
    /// The wire attaches to a node that already exists, and this apply may not rewire.
    WireTouchesExistingNode {
        from: String,
        to: String,
        wire_type: WireType,
        existing: Vec<String>,
    },
    /// The board is larger than the apply step will attempt.
    BoardTooLarge {
        nodes: usize,
        wires: usize,
        max_nodes: usize,
        max_wires: usize,
    },
    /// The wire matrix forbids this pair. Default-DENY: anything not listed is refused.
    WireNotAllowed {
        from: String,
        from_type: NodeType,
        to: String,
        to_type: NodeType,
        wire_type: WireType,
    },
    /// A node's name fails the §3 name contract.
    InvalidName { name: String, reason: String },
    /// A node's config fails `wheel_core::validate_config_with`.
    InvalidConfig { name: String, reason: String },
    /// The harness this build cannot run. The engine refuses a `codex` node at creation, so
    /// without this the board passed the preview and then died halfway through applying.
    UnsupportedHarness { name: String, harness: String },
    /// `remove` names a node that is not on the board.
    RemoveUnknownNode { name: String },
    /// `remove` names a wire that is not on the board.
    RemoveUnknownWire {
        from: String,
        to: String,
        wire_type: WireType,
    },
    /// The same node is both changed and removed.
    ConflictingChange { name: String },
    /// A wire would attach to a node this same board removes.
    WireToRemovedNode {
        from: String,
        to: String,
        wire_type: WireType,
        removed: String,
    },
    /// Removing a node destroys what it holds, and this apply was not given that consent.
    DeleteNotPermitted { name: String, node_type: NodeType },
    /// Removing a wire takes a capability away, and this apply was not given that consent.
    UnwireNotPermitted {
        from: String,
        to: String,
        wire_type: WireType,
    },
}

impl Refusal {
    /// One line a person can act on, naming the node or wire at fault.
    pub fn message(&self) -> String {
        match self {
            Refusal::MalformedBoard { reason } => {
                format!("the board could not be read: {reason}")
            }
            Refusal::DuplicateNodeName { name } => {
                format!("two nodes are both named {name:?}; names must be unique on a board")
            }
            Refusal::UnknownNode { wire_from, wire_to, missing } => format!(
                "the wire {wire_from:?} -> {wire_to:?} names {missing:?}, which is not on the board \
                 and is not being created"
            ),
            Refusal::SelfWire { node } => format!("{node:?} is wired to itself"),
            Refusal::NodeTypeMismatch { name, existing_type, emitted_type } => format!(
                "{name:?} is already on the board as a {}, and the board defines it as a {}; \
                 rename one of them — a node's type cannot be changed",
                existing_type.as_str(),
                emitted_type.as_str()
            ),
            Refusal::RenameNotSupported { id, existing_name, emitted_name } => format!(
                "the board keeps node {id}'s id but calls it {emitted_name:?} instead of \
                 {existing_name:?}; renaming is not something this step can apply — keep the \
                 existing name, or add a new node and remove the old one"
            ),
            Refusal::PatchNotPermitted { name } => format!(
                "{name:?} is already on the board and this apply may only create nodes; \
                 allow modifying existing nodes to change it"
            ),
            Refusal::WireTouchesExistingNode { from, to, wire_type, existing } => format!(
                "the wire {from:?} -> {to:?} ({}) attaches to {}, which already exists; \
                 wiring an existing node changes what it can do or what can reach it, \
                 so it needs the same consent as modifying one",
                wire_type.as_str(),
                existing
                    .iter()
                    .map(|n| format!("{n:?}"))
                    .collect::<Vec<_>>()
                    .join(" and ")
            ),
            Refusal::BoardTooLarge { nodes, wires, max_nodes, max_wires } => format!(
                "the board has {nodes} nodes and {wires} wires; this step applies at most \
                 {max_nodes} nodes and {max_wires} wires in one go"
            ),
            Refusal::WireNotAllowed { from, from_type, to, to_type, wire_type } => format!(
                "no {} wire is allowed from a {} to a {}: {from:?} -> {to:?}",
                wire_type.as_str(),
                from_type.as_str(),
                to_type.as_str()
            ),
            Refusal::InvalidName { name, reason } => {
                format!("{name:?} is not a valid node name: {reason}")
            }
            Refusal::InvalidConfig { name, reason } => {
                format!("{name:?}'s config is invalid: {reason}")
            }
            Refusal::UnsupportedHarness { name, harness } => format!(
                "{name:?} uses the {harness:?} harness, which this engine refuses to create; \
                 use claude"
            ),
            Refusal::RemoveUnknownNode { name } => format!(
                "the board asks to remove {name:?}, which is not on this board"
            ),
            Refusal::RemoveUnknownWire { from, to, wire_type } => format!(
                "the board asks to remove the wire {from:?} -> {to:?} ({}), which is not on this \
                 board",
                wire_type.as_str()
            ),
            Refusal::ConflictingChange { name } => format!(
                "the board both changes and removes {name:?}; it can do one or the other"
            ),
            Refusal::WireToRemovedNode { from, to, wire_type, removed } => format!(
                "the wire {from:?} -> {to:?} ({}) attaches to {removed:?}, which this same board \
                 removes",
                wire_type.as_str()
            ),
            Refusal::DeleteNotPermitted { name, node_type } => format!(
                "removing the {} {name:?} destroys what it holds, and this apply may not remove \
                 anything; allow removals to do it",
                node_type.as_str()
            ),
            Refusal::UnwireNotPermitted { from, to, wire_type } => format!(
                "removing the wire {from:?} -> {to:?} ({}) takes a capability away, and this apply \
                 may not remove wires; allow unwiring to do it",
                wire_type.as_str()
            ),
        }
    }
}

/// An existing node whose config a patch is diffed against.
#[derive(Debug, Clone, PartialEq)]
pub struct ExistingNode {
    pub id: Uuid,
    pub node_type: NodeType,
    /// The node's CURRENT config, so a patch can be the difference rather than a rewrite.
    pub config: serde_json::Value,
}

impl ExistingNode {
    /// An existing node whose config is not known — used where only identity matters.
    pub fn new(id: Uuid, node_type: NodeType) -> Self {
        Self {
            id,
            node_type,
            config: serde_json::Value::Null,
        }
    }
}

/// What is already on the board, as the apply step needs to see it.
#[derive(Debug, Clone, Default)]
pub struct ExistingBoard {
    pub nodes: HashMap<String, ExistingNode>,
    pub wires: Vec<(String, String, WireType)>,
}

impl ExistingBoard {
    fn name_of(&self, id: &str) -> Option<&String> {
        self.nodes
            .iter()
            .find(|(_, node)| node.id.to_string() == id)
            .map(|(name, _)| name)
    }
}

/// A node the plan will change, with the patch it will send.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PatchNode {
    pub name: String,
    #[serde(skip)]
    pub id: Uuid,
    /// The minimal merge patch: only the top-level keys whose value actually changes.
    pub config: serde_json::Value,
    /// Which fields those are, so the confirm step can say what changes rather than "it changes".
    pub fields: Vec<String>,
    /// Fields where a whole list is replaced. RFC 7386 has no element-wise array merge, so this is
    /// the one shape a user has to see coming: `workspaces` going from two entries to one is a
    /// loss, and it looks identical to an edit in every other respect.
    pub replaced_arrays: Vec<String>,
}

/// A node the plan will remove, and what goes with it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DeleteNode {
    pub name: String,
    #[serde(skip)]
    pub id: Uuid,
    #[serde(rename = "type")]
    pub node_type: NodeType,
    /// Wires that go when the node does. Shown under the node rather than as separate unwires,
    /// because they are a consequence of the deletion and not a second decision.
    pub wires: Vec<WireRef>,
}

/// What applying the board would do, once it is known to be legal.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Plan {
    pub create_nodes: Vec<EmittedNode>,
    pub patch_nodes: Vec<PatchNode>,
    pub create_wires: Vec<EmittedWire>,
    pub delete_wires: Vec<EmittedWire>,
    pub delete_nodes: Vec<DeleteNode>,
}

impl Plan {
    pub fn is_empty(&self) -> bool {
        self.create_nodes.is_empty()
            && self.patch_nodes.is_empty()
            && self.create_wires.is_empty()
            && self.delete_wires.is_empty()
            && self.delete_nodes.is_empty()
    }

    pub fn destroys(&self) -> bool {
        !self.delete_nodes.is_empty() || !self.delete_wires.is_empty()
    }

    /// A fingerprint of exactly what this plan would do, so consent can be bound to it.
    ///
    /// The board can change between the preview a user approved and the apply they pressed — by
    /// another tab, an agent, or the builder running again. Without this, the apply would go ahead
    /// with a plan recomputed against a board nobody looked at, and for a deletion that is the
    /// difference between removing what was shown and removing something else.
    pub fn digest(&self) -> String {
        let shape = serde_json::json!({
            "create_nodes": self.create_nodes.iter().map(|n| &n.name).collect::<Vec<_>>(),
            "patch_nodes": self.patch_nodes,
            "create_wires": self.create_wires,
            "delete_wires": self.delete_wires,
            "delete_nodes": self.delete_nodes,
        });
        format!("{:x}", Sha256::digest(shape.to_string().as_bytes()))
    }
}

/// What an apply is permitted to do, beyond being legal.
///
/// Four separate consents over four different risks. "You may rewrite this agent's prompt", "you
/// may put a public endpoint on its inbox", "you may take this capability away" and "you may
/// destroy this table's rows" are not the same permission, and granting one must not grant another.
#[derive(Debug, Clone, Copy, Default)]
pub struct ApplyPolicy {
    pub allow_patch: bool,
    pub allow_wire: bool,
    /// Allow removing NODES. Destroys what they hold.
    pub allow_delete: bool,
    /// Allow removing WIRES. Reversible, but takes a capability away.
    pub allow_unwire: bool,
}

pub const MAX_NODES: usize = 200;
pub const MAX_WIRES: usize = 1000;

/// Read a board that arrived as arbitrary JSON.
///
/// A parse failure is a refusal with a reason, not a framework-level 422 with a plain-text body:
/// the confirm step renders refusals, and "the board was refused" with nothing in it is the shape
/// that leaves a user with no idea what the builder got wrong.
pub fn read_board(value: serde_json::Value) -> Result<EmittedBoard, Refusal> {
    serde_json::from_value(value).map_err(|e| Refusal::MalformedBoard {
        reason: e.to_string(),
    })
}

/// Check an emitted board against the matrix and the current board.
///
/// Returns EVERY refusal, not the first: an LLM that got one wire wrong usually got several, and
/// handing them back one per round trip wastes the user's time.
pub fn validate(
    board: &EmittedBoard,
    existing: &ExistingBoard,
    policy: ApplyPolicy,
) -> Result<Plan, Vec<Refusal>> {
    let mut refusals = Vec::new();

    // Size first, and returned alone: every later check is per-node or per-wire, so an oversized
    // board would otherwise produce thousands of refusals nobody can read. Removals count — they
    // are engine calls too.
    let wire_count = board.wires.len()
        + board.nodes.iter().map(|n| n.wires.len()).sum::<usize>()
        + board.remove.wires.len();
    let node_count = board.nodes.len() + board.remove.nodes.len();
    if node_count > MAX_NODES || wire_count > MAX_WIRES {
        return Err(vec![Refusal::BoardTooLarge {
            nodes: node_count,
            wires: wire_count,
            max_nodes: MAX_NODES,
            max_wires: MAX_WIRES,
        }]);
    }

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

    for node in &board.nodes {
        let name_check = if node.config.node_type() == NodeType::Table {
            validate_table_name(&node.name)
        } else {
            validate_name(&node.name)
        };
        if let Err(e) = name_check {
            refusals.push(Refusal::InvalidName {
                name: node.name.clone(),
                reason: e.to_string(),
            });
        }
        // `allow_hosts: &[]` matches production: the engine always runs with an empty SSRF
        // allowlist, so this is the same answer the live create call would give.
        if let Err(e) = validate_config_with(&node.config, &[]) {
            refusals.push(Refusal::InvalidConfig {
                name: node.name.clone(),
                reason: e.to_string(),
            });
        }
        // The engine refuses a codex node outright (`reject_unsupported_harness`), so a board
        // carrying one is refused here rather than allowed to fail halfway through applying.
        if let NodeConfig::Agent(agent) = &node.config {
            if agent.harness != Harness::Claude {
                refusals.push(Refusal::UnsupportedHarness {
                    name: node.name.clone(),
                    harness: agent.harness.as_str().to_string(),
                });
            }
        }
        // An id that belongs to an existing node names THAT node, so the name must agree.
        if let Some(id) = &node.id {
            if let Some(existing_name) = existing.name_of(id) {
                if existing_name != &node.name {
                    refusals.push(Refusal::RenameNotSupported {
                        id: id.clone(),
                        existing_name: existing_name.clone(),
                        emitted_name: node.name.clone(),
                    });
                }
            }
        }
    }

    for node in &board.nodes {
        if let Some(present) = existing.nodes.get(&node.name) {
            let emitted_type = node.config.node_type();
            if present.node_type != emitted_type {
                refusals.push(Refusal::NodeTypeMismatch {
                    name: node.name.clone(),
                    existing_type: present.node_type,
                    emitted_type,
                });
            }
        }
    }
    for (name, node) in &existing.nodes {
        types.insert(name.as_str(), node.node_type);
    }

    // Nested wires are addressed by id; everything below works in names.
    let (mut wires, mut unresolved) = flatten_wires(board, existing);
    refusals.append(&mut unresolved);
    wires.extend(board.wires.iter().cloned());

    // Removals, before wires are judged: a wire to something this board removes is a different
    // mistake from a wire to something that never existed.
    let mut removed_nodes: Vec<String> = Vec::new();
    for name in &board.remove.nodes {
        match existing.nodes.get(name) {
            Some(_) => removed_nodes.push(name.clone()),
            None => refusals.push(Refusal::RemoveUnknownNode { name: name.clone() }),
        }
        if board.nodes.iter().any(|n| &n.name == name) {
            refusals.push(Refusal::ConflictingChange { name: name.clone() });
        }
    }

    for wire in &wires {
        if wire.from == wire.to {
            refusals.push(Refusal::SelfWire {
                node: wire.from.clone(),
            });
            continue;
        }
        if let Some(removed) = [&wire.from, &wire.to]
            .into_iter()
            .find(|n| removed_nodes.contains(n))
        {
            refusals.push(Refusal::WireToRemovedNode {
                from: wire.from.clone(),
                to: wire.to.clone(),
                wire_type: wire.wire_type,
                removed: removed.clone(),
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

    // Wires to remove must be wires that exist. Ones that a node deletion already takes are folded
    // into that deletion rather than attempted twice.
    let mut delete_wires = Vec::new();
    for wire in &board.remove.wires {
        let present = existing
            .wires
            .iter()
            .any(|(f, t, ty)| f == &wire.from && t == &wire.to && *ty == wire.wire_type);
        let cascaded = removed_nodes.contains(&wire.from) || removed_nodes.contains(&wire.to);
        if !present {
            refusals.push(Refusal::RemoveUnknownWire {
                from: wire.from.clone(),
                to: wire.to.clone(),
                wire_type: wire.wire_type,
            });
        } else if !cascaded {
            delete_wires.push(wire.clone());
        }
    }

    if !refusals.is_empty() {
        return Err(refusals);
    }

    let mut create_nodes = Vec::new();
    let mut patch_nodes = Vec::new();
    for node in &board.nodes {
        match existing.nodes.get(&node.name) {
            Some(present) => {
                // Read before write: the patch is the DIFFERENCE against what is stored, and a
                // node the board re-emits unchanged is not a change at all — so it neither needs
                // consent nor appears in the plan.
                let (config, fields, replaced_arrays) =
                    minimal_patch(&present.config, &node.raw_config);
                if fields.is_empty() {
                    continue;
                }
                if !policy.allow_patch {
                    refusals.push(Refusal::PatchNotPermitted {
                        name: node.name.clone(),
                    });
                    continue;
                }
                patch_nodes.push(PatchNode {
                    name: node.name.clone(),
                    id: present.id,
                    config,
                    fields,
                    replaced_arrays,
                });
            }
            None => create_nodes.push(node.clone()),
        }
    }

    let mut delete_nodes = Vec::new();
    for name in &removed_nodes {
        let present = &existing.nodes[name];
        if !policy.allow_delete {
            refusals.push(Refusal::DeleteNotPermitted {
                name: name.clone(),
                node_type: present.node_type,
            });
            continue;
        }
        delete_nodes.push(DeleteNode {
            name: name.clone(),
            id: present.id,
            node_type: present.node_type,
            wires: existing
                .wires
                .iter()
                .filter(|(f, t, _)| f == name || t == name)
                .map(|(f, t, ty)| WireRef {
                    from: f.clone(),
                    to: t.clone(),
                    wire_type: *ty,
                })
                .collect(),
        });
    }

    if !policy.allow_unwire {
        for wire in &delete_wires {
            refusals.push(Refusal::UnwireNotPermitted {
                from: wire.from.clone(),
                to: wire.to.clone(),
                wire_type: wire.wire_type,
            });
        }
    }

    if !refusals.is_empty() {
        return Err(refusals);
    }

    let create_wires: Vec<EmittedWire> = wires
        .iter()
        .filter(|w| {
            !existing
                .wires
                .iter()
                .any(|(f, t, ty)| f == &w.from && t == &w.to && *ty == w.wire_type)
        })
        .cloned()
        .collect();

    // ADVERSARY 050: a wire is the capability. Attaching one to a node that already exists changes
    // what that node can do without editing its config, so it needs the same consent that editing
    // it does. Checked after the duplicate filter, so re-applying an unchanged board stays a no-op.
    if !policy.allow_wire {
        for wire in &create_wires {
            let touched: Vec<String> = [&wire.from, &wire.to]
                .into_iter()
                .filter(|name| existing.nodes.contains_key(*name))
                .cloned()
                .collect();
            if !touched.is_empty() {
                refusals.push(Refusal::WireTouchesExistingNode {
                    from: wire.from.clone(),
                    to: wire.to.clone(),
                    wire_type: wire.wire_type,
                    existing: touched,
                });
            }
        }
        if !refusals.is_empty() {
            return Err(refusals);
        }
    }

    Ok(Plan {
        create_nodes,
        patch_nodes,
        create_wires,
        delete_wires,
        delete_nodes,
    })
}

/// Nested, id-addressed wires as flat, name-addressed ones.
///
/// An id may name a node this board creates or one already on the board — the builder rewires an
/// existing node by its id. An id that names neither is a refusal, never a dropped wire.
fn flatten_wires(
    board: &EmittedBoard,
    existing: &ExistingBoard,
) -> (Vec<EmittedWire>, Vec<Refusal>) {
    let mut by_id: HashMap<&str, &str> = HashMap::new();
    for node in &board.nodes {
        if let Some(id) = &node.id {
            by_id.insert(id.as_str(), node.name.as_str());
        }
    }

    let mut wires = Vec::new();
    let mut refusals = Vec::new();
    for node in &board.nodes {
        for wire in &node.wires {
            let target = by_id
                .get(wire.to.as_str())
                .map(|n| (*n).to_string())
                .or_else(|| existing.name_of(&wire.to).cloned());
            match target {
                Some(to) => wires.push(EmittedWire {
                    from: node.name.clone(),
                    to,
                    wire_type: wire.wire_type,
                }),
                None => refusals.push(Refusal::UnknownNode {
                    wire_from: node.name.clone(),
                    wire_to: wire.to.clone(),
                    missing: wire.to.clone(),
                }),
            }
        }
    }
    (wires, refusals)
}

/// RFC 7386, the same merge the engine applies, so what is planned is what will be stored.
fn merge_patch(target: &mut serde_json::Value, patch: &serde_json::Value) {
    let serde_json::Value::Object(patch_map) = patch else {
        *target = patch.clone();
        return;
    };
    if !target.is_object() {
        *target = serde_json::Value::Object(serde_json::Map::new());
    }
    let map = target.as_object_mut().expect("just made it an object");
    for (k, v) in patch_map {
        if v.is_null() {
            map.remove(k);
        } else {
            merge_patch(map.entry(k.clone()).or_insert(serde_json::Value::Null), v);
        }
    }
}

/// The smallest patch that turns `current` into what the board asks for, and what it changes.
///
/// Only the top-level keys whose merged value actually differs are sent. A field the board never
/// mentioned is not in the patch at all, so it keeps its stored value — which is the whole
/// difference between an improve that edits one prompt and one that resets an agent's config.
fn minimal_patch(
    current: &serde_json::Value,
    emitted: &serde_json::Value,
) -> (serde_json::Value, Vec<String>, Vec<String>) {
    let mut desired = current.clone();
    merge_patch(&mut desired, emitted);

    let mut patch = serde_json::Map::new();
    let mut fields = Vec::new();
    let mut replaced_arrays = Vec::new();
    let empty = serde_json::Map::new();
    let emitted_keys = emitted.as_object().unwrap_or(&empty);

    for key in emitted_keys.keys() {
        let before = current.get(key).unwrap_or(&serde_json::Value::Null);
        let after = desired.get(key).unwrap_or(&serde_json::Value::Null);
        if before == after {
            continue;
        }
        fields.push(key.clone());
        if before.is_array() && after.is_array() {
            replaced_arrays.push(key.clone());
        }
        patch.insert(key.clone(), emitted_keys[key].clone());
    }
    fields.sort();
    replaced_arrays.sort();
    (serde_json::Value::Object(patch), fields, replaced_arrays)
}

/// The board operations the apply step needs.
#[async_trait::async_trait]
pub trait BoardClient: Send + Sync {
    async fn create_node(&self, node: &EmittedNode) -> Result<Uuid, String>;
    async fn patch_config(&self, id: Uuid, config: &serde_json::Value) -> Result<(), String>;
    async fn add_wire(&self, from: Uuid, to: Uuid, wire_type: WireType) -> Result<(), String>;
    async fn delete_node(&self, id: Uuid) -> Result<(), String>;
    async fn delete_wire(&self, from: Uuid, to: Uuid, wire_type: WireType) -> Result<(), String>;
}

/// A wire, as a consumer needs it: addressable, not a sentence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WireRef {
    pub from: String,
    pub to: String,
    #[serde(rename = "type")]
    pub wire_type: WireType,
}

impl WireRef {
    pub fn of_emitted(w: &EmittedWire) -> Self {
        Self::of(w)
    }

    fn of(w: &EmittedWire) -> Self {
        Self {
            from: w.from.clone(),
            to: w.to.clone(),
            wire_type: w.wire_type,
        }
    }

    fn label(&self) -> String {
        format!("{} -> {} ({})", self.from, self.to, self.wire_type.as_str())
    }
}

/// One step that did not land, named so the report says which.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Failure {
    pub step: String,
    pub error: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wire: Option<WireRef>,
}

impl Failure {
    fn node(name: &str, step: String, error: String) -> Self {
        Self {
            step,
            error,
            node: Some(name.to_string()),
            wire: None,
        }
    }
    fn wire(w: &WireRef, error: String) -> Self {
        Self {
            step: format!("create wire {}", w.label()),
            error,
            node: None,
            wire: Some(w.clone()),
        }
    }

    pub fn step(step: impl Into<String>, error: impl Into<String>) -> Self {
        Self {
            step: step.into(),
            error: error.into(),
            node: None,
            wire: None,
        }
    }

    pub fn sandbox_did_not_start() -> Self {
        Self::step("sandbox", "the project engine did not become healthy")
    }

    pub fn capabilities(error: impl Into<String>) -> Self {
        Self::step("capabilities", error)
    }
}

/// Exactly what happened. Never "ok" for a partial apply — see `is_complete`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ApplyReport {
    pub created_nodes: Vec<String>,
    pub patched_nodes: Vec<String>,
    pub created_wires: Vec<WireRef>,
    pub deleted_nodes: Vec<String>,
    pub deleted_wires: Vec<WireRef>,
    pub failures: Vec<Failure>,
}

impl ApplyReport {
    /// The success-shape invariant. A caller that reports success must ask this, not whether the
    /// call returned.
    pub fn is_complete(&self) -> bool {
        self.failures.is_empty()
    }
}

/// Realise a validated plan.
///
/// The order is deliberate, and it is the safety property this step can actually offer:
///
/// 1. **unwire**, so capability is taken away before any is added;
/// 2. **create**, then **patch**, then **wire**, so a wire always has both endpoints;
/// 3. **delete nodes LAST, and only if everything above succeeded.**
///
/// Step 3's condition is the one that matters. Deleting is the only irreversible thing here, and a
/// half-applied improve that ALSO destroyed what it was replacing is the worst outcome available:
/// the user would be left with neither the old board nor the new one. So if anything earlier
/// failed, each planned deletion is reported as a failure that says nothing was destroyed.
///
/// This is NOT atomic and does not pretend to be. What it guarantees is that nothing ILLEGAL was
/// attempted, that the report names every step that failed, and that a failure never escalates
/// into data loss.
pub async fn execute(
    plan: &Plan,
    existing: &ExistingBoard,
    client: &dyn BoardClient,
) -> ApplyReport {
    let mut report = ApplyReport::default();
    let mut ids: HashMap<String, Uuid> = existing
        .nodes
        .iter()
        .map(|(name, n)| (name.clone(), n.id))
        .collect();

    for wire in &plan.delete_wires {
        let reference = WireRef::of(wire);
        let (Some(from), Some(to)) = (ids.get(&wire.from).copied(), ids.get(&wire.to).copied())
        else {
            report.failures.push(Failure {
                step: format!("remove wire {}", reference.label()),
                error: "one end of this wire is no longer on the board".into(),
                node: None,
                wire: Some(reference),
            });
            continue;
        };
        match client.delete_wire(from, to, wire.wire_type).await {
            Ok(()) => report.deleted_wires.push(reference),
            Err(error) => report.failures.push(Failure {
                step: format!("remove wire {}", reference.label()),
                error,
                node: None,
                wire: Some(reference),
            }),
        }
    }

    for node in &plan.create_nodes {
        match client.create_node(node).await {
            Ok(id) => {
                ids.insert(node.name.clone(), id);
                report.created_nodes.push(node.name.clone());
            }
            Err(error) => report.failures.push(Failure::node(
                &node.name,
                format!("create node {:?}", node.name),
                error,
            )),
        }
    }

    for node in &plan.patch_nodes {
        // Only `config` is sent, so the merge leaves name and position alone; and the body is the
        // difference computed against what was stored, not a re-serialised whole.
        let body = serde_json::json!({ "config": node.config });
        match client.patch_config(node.id, &body).await {
            Ok(()) => report.patched_nodes.push(node.name.clone()),
            Err(error) => report.failures.push(Failure::node(
                &node.name,
                format!("patch node {:?}", node.name),
                error,
            )),
        }
    }

    for wire in &plan.create_wires {
        let reference = WireRef::of(wire);
        let (Some(from), Some(to)) = (ids.get(&wire.from).copied(), ids.get(&wire.to).copied())
        else {
            let missing = if ids.contains_key(&wire.from) {
                &wire.to
            } else {
                &wire.from
            };
            report.failures.push(Failure::wire(
                &reference,
                format!("{missing:?} was not created, so this wire has no endpoint"),
            ));
            continue;
        };
        match client.add_wire(from, to, wire.wire_type).await {
            Ok(()) => report.created_wires.push(reference),
            Err(error) => report.failures.push(Failure::wire(&reference, error)),
        }
    }

    let something_failed = !report.failures.is_empty();
    for node in &plan.delete_nodes {
        if something_failed {
            report.failures.push(Failure::node(
                &node.name,
                format!("remove node {:?}", node.name),
                "not removed: an earlier step failed, so nothing was destroyed".into(),
            ));
            continue;
        }
        match client.delete_node(node.id).await {
            Ok(()) => report.deleted_nodes.push(node.name.clone()),
            Err(error) => report.failures.push(Failure::node(
                &node.name,
                format!("remove node {:?}", node.name),
                error,
            )),
        }
    }

    report
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

    /// A client that records what it was asked to do and fails whatever it was told to fail.
    struct FakeClient {
        fail_node: Option<String>,
        fail_wire: bool,
        created: std::sync::Mutex<Vec<String>>,
        patched: std::sync::Mutex<Vec<Uuid>>,
        patch_bodies: std::sync::Mutex<Vec<serde_json::Value>>,
        deleted_nodes: std::sync::Mutex<Vec<Uuid>>,
        deleted_wires: std::sync::Mutex<Vec<(Uuid, Uuid, WireType)>>,
    }
    impl FakeClient {
        fn new() -> Self {
            Self {
                fail_node: None,
                fail_wire: false,
                created: std::sync::Mutex::new(Vec::new()),
                patched: std::sync::Mutex::new(Vec::new()),
                patch_bodies: std::sync::Mutex::new(Vec::new()),
                deleted_nodes: std::sync::Mutex::new(Vec::new()),
                deleted_wires: std::sync::Mutex::new(Vec::new()),
            }
        }
    }
    #[async_trait::async_trait]
    impl BoardClient for FakeClient {
        async fn create_node(&self, node: &EmittedNode) -> Result<Uuid, String> {
            if self.fail_node.as_deref() == Some(node.name.as_str()) {
                return Err("engine said no".into());
            }
            self.created.lock().unwrap().push(node.name.clone());
            Ok(Uuid::new_v4())
        }
        async fn patch_config(&self, id: Uuid, c: &serde_json::Value) -> Result<(), String> {
            self.patched.lock().unwrap().push(id);
            self.patch_bodies.lock().unwrap().push(c.clone());
            Ok(())
        }
        async fn delete_node(&self, id: Uuid) -> Result<(), String> {
            self.deleted_nodes.lock().unwrap().push(id);
            Ok(())
        }
        async fn delete_wire(&self, f: Uuid, t: Uuid, w: WireType) -> Result<(), String> {
            self.deleted_wires.lock().unwrap().push((f, t, w));
            Ok(())
        }
        async fn add_wire(&self, _f: Uuid, _t: Uuid, _w: WireType) -> Result<(), String> {
            if self.fail_wire {
                return Err("engine refused the wire".into());
            }
            Ok(())
        }
    }

    #[tokio::test]
    async fn a_legal_board_applies_fully_and_reports_complete() {
        let b = board(serde_json::json!({
            "nodes": [agent("researcher"), ctx("notes")],
            "wires": [{"from": "notes", "to": "researcher", "type": "send"}],
        }));
        let existing = ExistingBoard::default();
        let plan = validate(
            &b,
            &existing,
            ApplyPolicy {
                allow_patch: true,
                allow_wire: true,
                ..Default::default()
            },
        )
        .expect("legal");
        let report = execute(&plan, &existing, &FakeClient::new()).await;

        assert!(report.is_complete(), "{report:?}");
        assert_eq!(report.created_nodes.len(), 2);
        assert_eq!(report.created_wires.len(), 1);
        assert!(report.failures.is_empty());
    }

    /// The success-shape invariant: a partial apply must never look complete.
    #[tokio::test]
    async fn a_partial_apply_is_never_reported_as_complete() {
        let b = board(serde_json::json!({
            "nodes": [agent("researcher"), ctx("notes")],
            "wires": [{"from": "notes", "to": "researcher", "type": "send"}],
        }));
        let existing = ExistingBoard::default();
        let plan = validate(
            &b,
            &existing,
            ApplyPolicy {
                allow_patch: true,
                allow_wire: true,
                ..Default::default()
            },
        )
        .expect("legal");
        let mut client = FakeClient::new();
        client.fail_node = Some("notes".into());
        let report = execute(&plan, &existing, &client).await;

        assert!(
            !report.is_complete(),
            "a failed node still reported complete"
        );
        assert_eq!(report.created_nodes, vec!["researcher".to_string()]);
        assert!(
            report.failures.iter().any(|f| f.step.contains("notes")),
            "{report:?}"
        );
    }

    /// A wire whose endpoint failed to create is reported as that, not as an engine 404 the user
    /// cannot interpret.
    #[tokio::test]
    async fn a_wire_whose_endpoint_failed_says_which_endpoint_is_missing() {
        let b = board(serde_json::json!({
            "nodes": [agent("researcher"), ctx("notes")],
            "wires": [{"from": "notes", "to": "researcher", "type": "send"}],
        }));
        let existing = ExistingBoard::default();
        let plan = validate(
            &b,
            &existing,
            ApplyPolicy {
                allow_patch: true,
                allow_wire: true,
                ..Default::default()
            },
        )
        .expect("legal");
        let mut client = FakeClient::new();
        client.fail_node = Some("notes".into());
        let report = execute(&plan, &existing, &client).await;

        let wire_failure = report
            .failures
            .iter()
            .find(|f| f.step.starts_with("create wire"))
            .expect("the wire must be reported, not silently skipped");
        assert!(wire_failure.error.contains("notes"), "{wire_failure:?}");
        assert!(
            wire_failure.error.contains("no endpoint"),
            "{wire_failure:?}"
        );
    }

    #[tokio::test]
    async fn an_engine_wire_refusal_is_reported_against_the_named_wire() {
        let b = board(serde_json::json!({
            "nodes": [agent("researcher"), ctx("notes")],
            "wires": [{"from": "notes", "to": "researcher", "type": "send"}],
        }));
        let existing = ExistingBoard::default();
        let plan = validate(
            &b,
            &existing,
            ApplyPolicy {
                allow_patch: true,
                allow_wire: true,
                ..Default::default()
            },
        )
        .expect("legal");
        let mut client = FakeClient::new();
        client.fail_wire = true;
        let report = execute(&plan, &existing, &client).await;

        assert!(!report.is_complete());
        assert!(report.created_wires.is_empty());
        let f = &report.failures[0];
        assert!(f.step.contains("notes -> researcher"), "{f:?}");
        assert!(f.error.contains("refused"), "{f:?}");
    }

    /// Improve: an existing node is PATCHed by its id, and a node the board never mentioned is
    /// never touched.
    #[tokio::test]
    async fn improve_patches_only_what_changed_and_touches_nothing_else() {
        let researcher = Uuid::new_v4();
        let mut existing = ExistingBoard::default();
        existing.nodes.insert(
            "researcher".into(),
            ExistingNode::new(researcher, NodeType::Agent),
        );
        existing.nodes.insert(
            "untouched".into(),
            ExistingNode::new(Uuid::new_v4(), NodeType::Ctx),
        );

        let b = board(serde_json::json!({"nodes": [agent("researcher")], "wires": []}));
        let plan = validate(
            &b,
            &existing,
            ApplyPolicy {
                allow_patch: true,
                allow_wire: true,
                ..Default::default()
            },
        )
        .expect("legal");
        let client = FakeClient::new();
        let report = execute(&plan, &existing, &client).await;

        assert!(report.is_complete(), "{report:?}");
        assert_eq!(report.patched_nodes, vec!["researcher".to_string()]);
        assert!(report.created_nodes.is_empty());
        let patched = client.patched.lock().unwrap().clone();
        assert_eq!(
            patched,
            vec![researcher],
            "patched the wrong node, or too many"
        );
    }

    /// ADVERSARY 049(1)+(2): a name colliding with an existing node of a DIFFERENT type used to
    /// become a silent patch. The engine rejects a type change, so it died mid-apply with a
    /// confusing partial board.
    #[test]
    fn a_name_colliding_with_a_different_type_is_refused_not_silently_patched() {
        let mut existing = ExistingBoard::default();
        existing.nodes.insert(
            "notes".into(),
            ExistingNode::new(Uuid::new_v4(), NodeType::Agent),
        );
        // The board calls "notes" a ctx; the board already has an AGENT by that name.
        let b = board(serde_json::json!({"nodes": [ctx("notes")], "wires": []}));

        let refusals = validate(&b, &existing, ApplyPolicy::default())
            .expect_err("a type change must be refused");
        let m = refusals[0].message();
        assert!(
            matches!(refusals[0], Refusal::NodeTypeMismatch { .. }),
            "{:?}",
            refusals[0]
        );
        assert!(m.contains("notes"), "{m}");
        assert!(
            m.contains("agent") && m.contains("ctx"),
            "both types must be named: {m}"
        );
    }

    /// The same name with the SAME type is the improve case and stays a patch — explicit in the
    /// plan, never mixed in with creates.
    #[test]
    fn the_same_name_with_the_same_type_is_a_patch_not_a_refusal() {
        let mut existing = ExistingBoard::default();
        existing.nodes.insert(
            "researcher".into(),
            ExistingNode::new(Uuid::new_v4(), NodeType::Agent),
        );
        let b = board(serde_json::json!({"nodes": [agent("researcher")], "wires": []}));

        let plan = validate(
            &b,
            &existing,
            ApplyPolicy {
                allow_patch: true,
                allow_wire: true,
                ..Default::default()
            },
        )
        .expect("same type is legal");
        assert_eq!(plan.patch_nodes.len(), 1);
        assert!(
            plan.create_nodes.is_empty(),
            "a patch must never be planned as a create"
        );
    }

    /// 049(2): with mismatches refused, a wire against a colliding name can never be validated
    /// against a type one side disagrees with — the ambiguity is gone rather than resolved.
    #[test]
    fn a_wire_is_never_validated_against_a_disputed_type() {
        let mut existing = ExistingBoard::default();
        existing.nodes.insert(
            "secrets".into(),
            ExistingNode::new(Uuid::new_v4(), NodeType::Vault),
        );
        // Emitted as a ctx, so ctx->agent send would LOOK legal if the emitted type won.
        let b = board(serde_json::json!({
            "nodes": [ctx("secrets"), agent("a")],
            "wires": [{"from": "secrets", "to": "a", "type": "send"}],
        }));
        let refusals = validate(&b, &existing, ApplyPolicy::default())
            .expect_err("the collision must be refused first");
        assert!(
            refusals
                .iter()
                .any(|r| matches!(r, Refusal::NodeTypeMismatch { .. })),
            "{refusals:?}"
        );
    }

    /// 049(3): a cheap bound, refused alone so the caller gets one readable line.
    #[test]
    fn an_oversized_board_is_refused_with_one_readable_refusal() {
        let nodes: Vec<_> = (0..=MAX_NODES).map(|i| agent(&format!("a{i}"))).collect();
        let b = board(serde_json::json!({"nodes": nodes, "wires": []}));

        let refusals =
            validate(&b, &ExistingBoard::default(), ApplyPolicy::default()).expect_err("too large");
        assert_eq!(
            refusals.len(),
            1,
            "an oversized board must not emit one refusal per node"
        );
        let m = refusals[0].message();
        assert!(m.contains(&MAX_NODES.to_string()), "{m}");
        assert!(matches!(refusals[0], Refusal::BoardTooLarge { .. }));
    }

    #[test]
    fn a_board_at_the_cap_is_still_accepted() {
        let nodes: Vec<_> = (0..MAX_NODES).map(|i| agent(&format!("a{i}"))).collect();
        let b = board(serde_json::json!({"nodes": nodes, "wires": []}));
        let plan = validate(&b, &ExistingBoard::default(), ApplyPolicy::default())
            .expect("exactly at the cap is legal");
        assert_eq!(plan.create_nodes.len(), MAX_NODES);
    }

    #[test]
    fn too_many_wires_is_refused_too() {
        let b = board(serde_json::json!({
            "nodes": [agent("a"), ctx("c")],
            "wires": (0..=MAX_WIRES)
                .map(|_| serde_json::json!({"from": "c", "to": "a", "type": "send"}))
                .collect::<Vec<_>>(),
        }));
        let refusals = validate(&b, &ExistingBoard::default(), ApplyPolicy::default())
            .expect_err("too many wires");
        assert!(
            matches!(refusals[0], Refusal::BoardTooLarge { .. }),
            "{:?}",
            refusals[0]
        );
    }

    /// SDK's point: the default must not be "modify whatever this board happens to name". A board
    /// that merely MENTIONS an existing node is refused unless the caller asked for patching.
    #[test]
    fn an_existing_node_is_not_modified_unless_the_caller_allowed_it() {
        let mut existing = ExistingBoard::default();
        existing.nodes.insert(
            "researcher".into(),
            ExistingNode::new(Uuid::new_v4(), NodeType::Agent),
        );
        let b = board(serde_json::json!({"nodes": [agent("researcher")], "wires": []}));

        let refusals = validate(&b, &existing, ApplyPolicy::default())
            .expect_err("create-only must refuse to modify");
        assert!(
            matches!(refusals[0], Refusal::PatchNotPermitted { .. }),
            "{:?}",
            refusals[0]
        );
        assert!(
            refusals[0].message().contains("researcher"),
            "{}",
            refusals[0].message()
        );
    }

    /// And the refusal names EVERY node it would have touched, so the confirm step can show the
    /// user exactly what they are being asked to allow.
    #[test]
    fn the_refusal_names_every_node_it_would_have_modified() {
        let mut existing = ExistingBoard::default();
        for name in ["a", "b"] {
            existing.nodes.insert(
                name.into(),
                ExistingNode::new(Uuid::new_v4(), NodeType::Agent),
            );
        }
        let b = board(serde_json::json!({"nodes": [agent("a"), agent("b")], "wires": []}));

        let refusals = validate(&b, &existing, ApplyPolicy::default()).expect_err("both refused");
        assert_eq!(refusals.len(), 2, "{refusals:?}");
        let named: Vec<_> = refusals
            .iter()
            .map(|r| match r {
                Refusal::PatchNotPermitted { name } => name.clone(),
                other => panic!("{other:?}"),
            })
            .collect();
        assert!(named.contains(&"a".to_string()) && named.contains(&"b".to_string()));
    }

    #[test]
    fn allowing_it_makes_the_same_board_a_patch() {
        let mut existing = ExistingBoard::default();
        existing.nodes.insert(
            "researcher".into(),
            ExistingNode::new(Uuid::new_v4(), NodeType::Agent),
        );
        let b = board(serde_json::json!({"nodes": [agent("researcher")], "wires": []}));

        let plan = validate(
            &b,
            &existing,
            ApplyPolicy {
                allow_patch: true,
                allow_wire: true,
                ..Default::default()
            },
        )
        .expect("opted in");
        assert_eq!(plan.patch_nodes.len(), 1);
    }

    /// ADVERSARY 050. This test used to assert the OPPOSITE — that create-only still allowed wiring
    /// an existing node — on my reasoning that "wiring to something is not changing it". That was
    /// wrong, and I had told both Web and ADVERSARY it was safe.
    ///
    /// A wire IS the capability. `ctx -> agent (send)` injects into that agent's prompt for good,
    /// with its config untouched, so create-only was protecting the wrong half.
    #[test]
    fn create_only_refuses_to_wire_an_existing_node() {
        let mut existing = ExistingBoard::default();
        existing.nodes.insert(
            "pm".into(),
            ExistingNode::new(Uuid::new_v4(), NodeType::Agent),
        );
        // The escalation from the finding: a new ctx, wired into an existing agent's prompt.
        let b = board(serde_json::json!({
            "nodes": [ctx("evil")],
            "wires": [{"from": "evil", "to": "pm", "type": "send"}],
        }));

        let refusals = validate(&b, &existing, ApplyPolicy::default())
            .expect_err("wiring an existing agent must need consent");
        let m = refusals[0].message();
        assert!(
            matches!(refusals[0], Refusal::WireTouchesExistingNode { .. }),
            "{:?}",
            refusals[0]
        );
        assert!(
            m.contains("pm"),
            "the refusal must name the existing node: {m}"
        );
    }

    /// The other direction is equally a change: an outbound wire grants the existing node reach it
    /// did not have. `pm -> vault (read)` hands it secrets without editing it.
    #[test]
    fn an_outbound_wire_from_an_existing_node_also_needs_consent() {
        let mut existing = ExistingBoard::default();
        existing.nodes.insert(
            "pm".into(),
            ExistingNode::new(Uuid::new_v4(), NodeType::Agent),
        );
        let b = board(serde_json::json!({
            "nodes": [vault("secrets")],
            "wires": [{"from": "pm", "to": "secrets", "type": "read"}],
        }));

        let refusals = validate(&b, &existing, ApplyPolicy::default())
            .expect_err("granting an existing agent new reach must need consent");
        assert!(
            refusals[0].message().contains("pm"),
            "{}",
            refusals[0].message()
        );
    }

    /// A wire between two nodes THIS board creates changes nothing pre-existing, so it is
    /// consent-free. Without this the gate would make the common case — build a new board — need a
    /// flag for no reason.
    #[test]
    fn a_wire_between_two_new_nodes_needs_no_consent() {
        let b = board(serde_json::json!({
            "nodes": [agent("researcher"), ctx("notes")],
            "wires": [{"from": "notes", "to": "researcher", "type": "send"}],
        }));
        let plan = validate(&b, &ExistingBoard::default(), ApplyPolicy::default())
            .expect("new to new is consent-free");
        assert_eq!(plan.create_wires.len(), 1);
    }

    #[test]
    fn allowing_wiring_lets_the_same_board_through() {
        let mut existing = ExistingBoard::default();
        existing.nodes.insert(
            "pm".into(),
            ExistingNode::new(Uuid::new_v4(), NodeType::Agent),
        );
        let b = board(serde_json::json!({
            "nodes": [ctx("evil")],
            "wires": [{"from": "evil", "to": "pm", "type": "send"}],
        }));
        let plan = validate(
            &b,
            &existing,
            ApplyPolicy {
                allow_patch: false,
                allow_wire: true,
                ..Default::default()
            },
        )
        .expect("opted in");
        assert_eq!(plan.create_wires.len(), 1);
    }

    /// The two consents are separate on purpose: rewriting a prompt and putting a public endpoint
    /// on an inbox are different risks, and granting one must not grant the other.
    #[test]
    fn allowing_config_changes_does_not_also_allow_rewiring() {
        let mut existing = ExistingBoard::default();
        existing.nodes.insert(
            "pm".into(),
            ExistingNode::new(Uuid::new_v4(), NodeType::Agent),
        );
        let b = board(serde_json::json!({
            "nodes": [ctx("evil")],
            "wires": [{"from": "evil", "to": "pm", "type": "send"}],
        }));
        let refusals = validate(
            &b,
            &existing,
            ApplyPolicy {
                allow_patch: true,
                allow_wire: false,
                ..Default::default()
            },
        )
        .expect_err("allow_patch must not imply allow_wire");
        assert!(matches!(
            refusals[0],
            Refusal::WireTouchesExistingNode { .. }
        ));
    }

    /// Re-applying an unchanged board must stay a no-op, not start demanding consent for wires that
    /// are already there — which is why the gate runs after the duplicate filter.
    #[test]
    fn an_existing_wire_does_not_re_ask_for_consent() {
        let mut existing = ExistingBoard::default();
        existing.nodes.insert(
            "notes".into(),
            ExistingNode::new(Uuid::new_v4(), NodeType::Ctx),
        );
        existing.nodes.insert(
            "pm".into(),
            ExistingNode::new(Uuid::new_v4(), NodeType::Agent),
        );
        existing
            .wires
            .push(("notes".into(), "pm".into(), WireType::Send));

        let b = board(serde_json::json!({
            "nodes": [],
            "wires": [{"from": "notes", "to": "pm", "type": "send"}],
        }));
        let plan = validate(&b, &existing, ApplyPolicy::default())
            .expect("an unchanged board must not need consent");
        assert!(plan.is_empty());
    }

    /// ADVERSARY's ask: a divergence between this layer's pre-validation and the matrix the ENGINE
    /// enforces should be a red build, not a runtime surprise.
    ///
    /// Exhaustive over every (from-type, to-type, wire-type) triple — 9 x 9 x 3 = 243 — asserting
    /// that this module's verdict is exactly `check_wire`'s. Both read wheel-core today, so they
    /// cannot disagree; this test is what makes that a FACT rather than a habit. The day someone
    /// special-cases a pair here, or stops calling wheel-core, this goes red naming the triple.
    ///
    /// It pins the direction that matters: the engine is the authority, and this layer exists only
    /// to refuse early. Accepting something the engine would refuse is the failure; refusing
    /// something it would accept is also caught, because the two verdicts must be equal.
    #[test]
    fn this_layer_agrees_with_the_engines_matrix_on_every_possible_wire() {
        fn config_for(t: NodeType) -> serde_json::Value {
            match t {
                NodeType::Agent => {
                    serde_json::json!({"harness": "claude", "system_prompt": "s"})
                }
                NodeType::Ctx => serde_json::json!({"markdown": "m"}),
                NodeType::Table => {
                    serde_json::json!({"columns": [{"name": "value", "type": "text"}]})
                }
                NodeType::Endpoint => {
                    serde_json::json!({"method": "GET", "path": "/p",
                                       "response_mode": "ack", "auth": {"mode": "none"}})
                }
                NodeType::Script => serde_json::json!({"language": "python", "source": "x"}),
                NodeType::Mcp => serde_json::json!({"transport": "stdio", "command": "c"}),
                NodeType::Vault => serde_json::json!({"keys": []}),
                NodeType::Chest => serde_json::json!({}),
                NodeType::Tool => serde_json::json!({
                    "kind": "http",
                    "source": {"format": "manual", "raw": "{}", "imported_at": "2026-01-01T00:00:00Z"},
                    "base_url": "https://example.test", "operations": []
                }),
            }
        }

        let mut checked = 0usize;
        for from in NodeType::ALL {
            for to in NodeType::ALL {
                for wire_type in [WireType::Read, WireType::Write, WireType::Send] {
                    let b = board(serde_json::json!({
                        "nodes": [
                            {"name": "src", "type": from.as_str(), "config": config_for(from)},
                            {"name": "dst", "type": to.as_str(), "config": config_for(to)},
                        ],
                        "wires": [{"from": "src", "to": "dst", "type": wire_type}],
                    }));

                    let mine =
                        validate(&b, &ExistingBoard::default(), ApplyPolicy::default()).is_ok();
                    // The engine's own gate. Distinct ids, so SelfWire is never the reason.
                    let engine =
                        wheel_core::check_wire(Uuid::new_v4(), from, Uuid::new_v4(), to, wire_type)
                            .is_ok();

                    assert_eq!(
                        mine,
                        engine,
                        "divergence on {} -> {} ({}): this layer says {}, the engine says {}",
                        from.as_str(),
                        to.as_str(),
                        wire_type.as_str(),
                        if mine { "allow" } else { "refuse" },
                        if engine { "allow" } else { "refuse" },
                    );
                    checked += 1;
                }
            }
        }
        assert_eq!(checked, 9 * 9 * 3, "the matrix stopped being exhaustive");
    }

    #[test]
    fn a_legal_board_plans_every_node_and_wire() {
        let b = board(serde_json::json!({
            "nodes": [agent("researcher"), ctx("notes")],
            "wires": [{"from": "notes", "to": "researcher", "type": "send"}],
        }));
        let plan =
            validate(&b, &ExistingBoard::default(), ApplyPolicy::default()).expect("a legal board");
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
        let refusals = validate(&b, &ExistingBoard::default(), ApplyPolicy::default())
            .expect_err("must be refused");
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
        let refusals = validate(&b, &ExistingBoard::default(), ApplyPolicy::default())
            .expect_err("three problems");
        assert_eq!(refusals.len(), 3, "{refusals:?}");
    }

    #[test]
    fn a_wire_to_a_node_that_is_neither_emitted_nor_present_is_refused_naming_it() {
        let b = board(serde_json::json!({
            "nodes": [agent("a")],
            "wires": [{"from": "a", "to": "nowhere", "type": "send"}],
        }));
        let r = validate(&b, &ExistingBoard::default(), ApplyPolicy::default())
            .expect_err("unknown node");
        assert!(r[0].message().contains("nowhere"), "{}", r[0].message());
    }

    #[test]
    fn a_node_wired_to_itself_is_refused() {
        let b = board(serde_json::json!({
            "nodes": [agent("a")],
            "wires": [{"from": "a", "to": "a", "type": "send"}],
        }));
        let r =
            validate(&b, &ExistingBoard::default(), ApplyPolicy::default()).expect_err("self wire");
        assert!(matches!(r[0], Refusal::SelfWire { .. }), "{:?}", r[0]);
    }

    #[test]
    fn two_nodes_with_one_name_are_refused_because_a_wire_naming_it_is_ambiguous() {
        let b = board(serde_json::json!({
            "nodes": [agent("twin"), ctx("twin")],
            "wires": [],
        }));
        let r =
            validate(&b, &ExistingBoard::default(), ApplyPolicy::default()).expect_err("duplicate");
        assert!(
            matches!(r[0], Refusal::DuplicateNodeName { .. }),
            "{:?}",
            r[0]
        );
    }

    /// A wire may name an existing node — with consent. Its legality is unchanged; what changed is
    /// that attaching to it now requires permission.
    #[test]
    fn a_wire_may_reference_a_node_already_on_the_board() {
        let mut existing = ExistingBoard::default();
        existing.nodes.insert(
            "researcher".into(),
            ExistingNode::new(Uuid::new_v4(), NodeType::Agent),
        );
        let b = board(serde_json::json!({
            "nodes": [ctx("notes")],
            "wires": [{"from": "notes", "to": "researcher", "type": "send"}],
        }));
        let plan = validate(
            &b,
            &existing,
            ApplyPolicy {
                allow_patch: false,
                allow_wire: true,
                ..Default::default()
            },
        )
        .expect("legal against the current board, once wiring is permitted");
        assert_eq!(plan.create_nodes.len(), 1);
        assert_eq!(plan.create_wires.len(), 1);
    }

    /// PM's third: improve applies only the delta. An existing node becomes a PATCH — a merge, so
    /// the fields the board does not mention keep their values — and an untouched node is absent
    /// from the plan entirely.
    #[test]
    fn improve_plans_only_the_delta_and_leaves_untouched_nodes_alone() {
        let mut existing = ExistingBoard::default();
        existing.nodes.insert(
            "researcher".into(),
            ExistingNode::new(Uuid::new_v4(), NodeType::Agent),
        );
        existing.nodes.insert(
            "untouched".into(),
            ExistingNode::new(Uuid::new_v4(), NodeType::Ctx),
        );

        let b = board(serde_json::json!({
            "nodes": [agent("researcher"), ctx("new-notes")],
            "wires": [{"from": "new-notes", "to": "researcher", "type": "send"}],
        }));
        let plan = validate(
            &b,
            &existing,
            ApplyPolicy {
                allow_patch: true,
                allow_wire: true,
                ..Default::default()
            },
        )
        .expect("legal");

        assert_eq!(plan.patch_nodes.len(), 1);
        assert_eq!(plan.patch_nodes[0].name, "researcher");
        assert_eq!(plan.create_nodes.len(), 1);
        assert_eq!(plan.create_nodes[0].name, "new-notes");
        let named: Vec<&str> = plan
            .create_nodes
            .iter()
            .map(|n| n.name.as_str())
            .chain(plan.patch_nodes.iter().map(|n| n.name.as_str()))
            .collect();
        assert!(
            !named.contains(&"untouched"),
            "a node the board never mentioned appeared in the plan: {named:?}"
        );
    }

    /// Applying the same board twice must not stack duplicate wires.
    #[test]
    fn a_wire_that_already_exists_is_not_created_again() {
        let mut existing = ExistingBoard::default();
        existing.nodes.insert(
            "notes".into(),
            ExistingNode::new(Uuid::new_v4(), NodeType::Ctx),
        );
        existing.nodes.insert(
            "researcher".into(),
            ExistingNode::new(Uuid::new_v4(), NodeType::Agent),
        );
        existing
            .wires
            .push(("notes".into(), "researcher".into(), WireType::Send));

        let b = board(serde_json::json!({
            "nodes": [],
            "wires": [{"from": "notes", "to": "researcher", "type": "send"}],
        }));
        let plan = validate(
            &b,
            &existing,
            ApplyPolicy {
                allow_patch: true,
                allow_wire: true,
                ..Default::default()
            },
        )
        .expect("legal");
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
        assert!(
            validate(&b, &ExistingBoard::default(), ApplyPolicy::default())
                .expect("legal")
                .is_empty()
        );
    }

    // --- the builder's own board shape -------------------------------------

    /// The defect this whole path was built on: the builder nests wires on the node, addressed by
    /// id (`BUILDER_PROMPT.md`), and a flat-only reader ignored them silently. Every builder board
    /// applied as nodes with NO wires and reported `applied: true`.
    #[test]
    fn a_builders_nested_wires_reach_the_plan() {
        let b = board(serde_json::json!({
            "project": {"id": "3f1a2b9c-0d4e-4a6b-8c1d-2e3f4a5b6c7d"},
            "nodes": [
                {"id": "11111111-1111-4111-8111-111111111111", "name": "notes", "type": "ctx",
                 "config": {"markdown": "n"},
                 "wires": [{"to": "22222222-2222-4222-8222-222222222222", "type": "send"}]},
                {"id": "22222222-2222-4222-8222-222222222222", "name": "worker", "type": "agent",
                 "config": {"harness": "claude", "system_prompt": "p"}, "wires": []}
            ]
        }));
        let plan = validate(&b, &ExistingBoard::default(), ApplyPolicy::default()).expect("legal");
        assert_eq!(plan.create_nodes.len(), 2);
        assert_eq!(
            plan.create_wires.len(),
            1,
            "the nested wire was dropped: {plan:?}"
        );
        assert_eq!(plan.create_wires[0].from, "notes");
        assert_eq!(plan.create_wires[0].to, "worker");
    }

    /// A nested wire naming an id that is nowhere is a REFUSAL. Dropping it would hand back a
    /// board that looks applied and is missing a connection nobody mentioned.
    #[test]
    fn a_nested_wire_to_an_unknown_id_is_refused_rather_than_dropped() {
        let b = board(serde_json::json!({
            "nodes": [{"id": "11111111-1111-4111-8111-111111111111", "name": "notes", "type": "ctx",
                       "config": {"markdown": "n"},
                       "wires": [{"to": "99999999-9999-4999-8999-999999999999", "type": "send"}]}]
        }));
        let refusals = validate(&b, &ExistingBoard::default(), ApplyPolicy::default())
            .expect_err("an unresolvable wire must be refused");
        assert!(
            matches!(refusals[0], Refusal::UnknownNode { .. }),
            "{:?}",
            refusals[0]
        );
    }

    /// Improve rewires an EXISTING node by the id the builder was shown.
    #[test]
    fn a_nested_wire_may_name_a_node_already_on_the_board_by_its_id() {
        let existing_id = Uuid::new_v4();
        let mut existing = ExistingBoard::default();
        existing
            .nodes
            .insert("pm".into(), ExistingNode::new(existing_id, NodeType::Agent));

        let b = board(serde_json::json!({
            "nodes": [{"id": "11111111-1111-4111-8111-111111111111", "name": "notes", "type": "ctx",
                       "config": {"markdown": "n"},
                       "wires": [{"to": existing_id.to_string(), "type": "send"}]}]
        }));
        let plan = validate(
            &b,
            &existing,
            ApplyPolicy {
                allow_wire: true,
                ..Default::default()
            },
        )
        .expect("legal once wiring an existing node is permitted");
        assert_eq!(plan.create_wires.len(), 1);
        assert_eq!(plan.create_wires[0].to, "pm");
    }

    // --- read before write --------------------------------------------------

    fn existing_agent(config: serde_json::Value) -> (Uuid, ExistingBoard) {
        let id = Uuid::new_v4();
        let mut existing = ExistingBoard::default();
        existing.nodes.insert(
            "researcher".into(),
            ExistingNode {
                id,
                node_type: NodeType::Agent,
                config,
            },
        );
        (id, existing)
    }

    /// The clobber this step used to cause. A typed re-serialisation turns an OMITTED
    /// `run_on_startup` into an explicit `false`, and the merge writes it over the stored `true`.
    /// The patch has to be the board's own words, diffed against what is stored.
    #[test]
    fn a_patch_never_writes_a_field_the_board_did_not_mention() {
        let (_, existing) = existing_agent(serde_json::json!({
            "harness": "claude",
            "system_prompt": "old",
            "run_on_startup": true,
            "ephemeral_context": true,
        }));
        let b = board(serde_json::json!({
            "nodes": [{"name": "researcher", "type": "agent",
                       "config": {"harness": "claude", "system_prompt": "new"}}]
        }));
        let plan = validate(
            &b,
            &existing,
            ApplyPolicy {
                allow_patch: true,
                ..Default::default()
            },
        )
        .expect("legal");

        let patch = &plan.patch_nodes[0];
        assert_eq!(patch.fields, vec!["system_prompt".to_string()]);
        assert_eq!(patch.config["system_prompt"], "new");
        assert!(
            patch.config.get("run_on_startup").is_none(),
            "a field the board never wrote is in the patch: {}",
            patch.config
        );
        assert!(
            patch.config.get("ephemeral_context").is_none(),
            "{}",
            patch.config
        );
    }

    /// An improve that re-states a node unchanged is not a change: it must not appear in the plan
    /// and must not demand the consent that changing one does.
    #[test]
    fn a_node_re_emitted_unchanged_is_not_a_patch_at_all() {
        let (_, existing) = existing_agent(serde_json::json!({
            "harness": "claude", "system_prompt": "same", "run_on_startup": true,
        }));
        let b = board(serde_json::json!({
            "nodes": [{"name": "researcher", "type": "agent",
                       "config": {"harness": "claude", "system_prompt": "same"}}]
        }));
        // Create-only: an unchanged node must not be refused for wanting to modify something.
        let plan =
            validate(&b, &existing, ApplyPolicy::default()).expect("an unchanged node is a no-op");
        assert!(plan.is_empty(), "{plan:?}");
    }

    /// RFC 7386 replaces a whole array, so the one thing a user must see coming is a list getting
    /// shorter — it looks exactly like any other edit otherwise.
    #[test]
    fn replacing_a_whole_list_is_called_out_as_such() {
        let (_, existing) = existing_agent(serde_json::json!({
            "harness": "claude",
            "system_prompt": "p",
            "workspaces": [{"path": "a", "git": {"url": "https://example.test/a.git"}},
                           {"path": "b", "git": {"url": "https://example.test/b.git"}}],
        }));
        let b = board(serde_json::json!({
            "nodes": [{"name": "researcher", "type": "agent", "config": {
                "harness": "claude", "system_prompt": "p",
                "workspaces": [{"path": "a", "git": {"url": "https://example.test/a.git"}}]
            }}]
        }));
        let plan = validate(
            &b,
            &existing,
            ApplyPolicy {
                allow_patch: true,
                ..Default::default()
            },
        )
        .expect("legal");
        assert_eq!(
            plan.patch_nodes[0].replaced_arrays,
            vec!["workspaces".to_string()]
        );
    }

    // --- capability truth ---------------------------------------------------

    /// The engine refuses a codex node at creation, so a board carrying one must be refused here —
    /// otherwise it previews cleanly and then dies half-applied.
    #[test]
    fn a_codex_agent_is_refused_before_anything_is_created() {
        let b = board(serde_json::json!({
            "nodes": [{"name": "worker", "type": "agent",
                       "config": {"harness": "codex", "system_prompt": "p"}}]
        }));
        let refusals = validate(&b, &ExistingBoard::default(), ApplyPolicy::default())
            .expect_err("codex is not runnable");
        let message = refusals[0].message();
        assert!(
            matches!(refusals[0], Refusal::UnsupportedHarness { .. }),
            "{:?}",
            refusals[0]
        );
        assert!(
            message.contains("worker") && message.contains("claude"),
            "{message}"
        );
    }

    #[test]
    fn keeping_an_existing_id_under_a_new_name_is_refused_rather_than_guessed() {
        let id = Uuid::new_v4();
        let mut existing = ExistingBoard::default();
        existing
            .nodes
            .insert("old".into(), ExistingNode::new(id, NodeType::Ctx));
        let b = board(serde_json::json!({
            "nodes": [{"id": id.to_string(), "name": "new", "type": "ctx", "config": {"markdown": "m"}}]
        }));
        let refusals = validate(&b, &existing, ApplyPolicy::default()).expect_err("a rename");
        assert!(
            refusals
                .iter()
                .any(|r| matches!(r, Refusal::RenameNotSupported { .. })),
            "{refusals:?}"
        );
    }

    // --- removals -----------------------------------------------------------

    fn board_with_two() -> ExistingBoard {
        let mut existing = ExistingBoard::default();
        existing.nodes.insert(
            "notes".into(),
            ExistingNode::new(Uuid::new_v4(), NodeType::Ctx),
        );
        existing.nodes.insert(
            "results".into(),
            ExistingNode::new(Uuid::new_v4(), NodeType::Table),
        );
        existing
            .wires
            .push(("notes".into(), "results".into(), WireType::Write));
        existing
    }

    /// Omission is not a deletion. A board that simply stops mentioning a node — an LLM
    /// abbreviating, or one talked into leaving it out — must change nothing.
    #[test]
    fn a_node_the_board_stops_mentioning_is_left_alone() {
        let existing = board_with_two();
        let b = board(serde_json::json!({"nodes": [], "wires": []}));
        let plan = validate(
            &b,
            &existing,
            ApplyPolicy {
                allow_delete: true,
                allow_unwire: true,
                ..Default::default()
            },
        )
        .expect("legal");
        assert!(plan.is_empty(), "omission proposed a change: {plan:?}");
    }

    #[test]
    fn removing_a_node_needs_its_own_consent_and_names_what_it_destroys() {
        let existing = board_with_two();
        let b = board(serde_json::json!({"nodes": [], "remove": {"nodes": ["results"]}}));

        let refusals =
            validate(&b, &existing, ApplyPolicy::default()).expect_err("removing must not be free");
        match &refusals[0] {
            Refusal::DeleteNotPermitted { name, node_type } => {
                assert_eq!(name, "results");
                assert_eq!(
                    *node_type,
                    NodeType::Table,
                    "the UI says what a table loses"
                );
            }
            other => panic!("{other:?}"),
        }

        // Granting patching or wiring is NOT granting destruction.
        assert!(validate(
            &b,
            &existing,
            ApplyPolicy {
                allow_patch: true,
                allow_wire: true,
                ..Default::default()
            }
        )
        .is_err());

        let plan = validate(
            &b,
            &existing,
            ApplyPolicy {
                allow_delete: true,
                ..Default::default()
            },
        )
        .expect("with consent");
        assert_eq!(plan.delete_nodes.len(), 1);
        assert_eq!(plan.delete_nodes[0].node_type, NodeType::Table);
        assert_eq!(
            plan.delete_nodes[0].wires.len(),
            1,
            "the wires that go with it are shown under it"
        );
    }

    #[test]
    fn removing_a_wire_needs_its_own_consent_too() {
        let existing = board_with_two();
        let b = board(serde_json::json!({
            "nodes": [],
            "remove": {"wires": [{"from": "notes", "to": "results", "type": "write"}]}
        }));
        let refusals = validate(&b, &existing, ApplyPolicy::default()).expect_err("consent");
        assert!(
            matches!(refusals[0], Refusal::UnwireNotPermitted { .. }),
            "{:?}",
            refusals[0]
        );

        // Being allowed to DESTROY a node does not imply being allowed to unwire, and vice versa.
        assert!(validate(
            &b,
            &existing,
            ApplyPolicy {
                allow_delete: true,
                ..Default::default()
            }
        )
        .is_err());

        let plan = validate(
            &b,
            &existing,
            ApplyPolicy {
                allow_unwire: true,
                ..Default::default()
            },
        )
        .expect("with consent");
        assert_eq!(plan.delete_wires.len(), 1);
    }

    #[test]
    fn a_removal_of_something_that_is_not_there_is_refused() {
        let existing = board_with_two();
        let b = board(serde_json::json!({
            "nodes": [],
            "remove": {"nodes": ["ghost"],
                       "wires": [{"from": "results", "to": "notes", "type": "read"}]}
        }));
        let refusals = validate(
            &b,
            &existing,
            ApplyPolicy {
                allow_delete: true,
                allow_unwire: true,
                ..Default::default()
            },
        )
        .expect_err("neither exists");
        assert!(
            refusals
                .iter()
                .any(|r| matches!(r, Refusal::RemoveUnknownNode { .. })),
            "{refusals:?}"
        );
        assert!(
            refusals
                .iter()
                .any(|r| matches!(r, Refusal::RemoveUnknownWire { .. })),
            "{refusals:?}"
        );
    }

    #[test]
    fn a_board_that_both_changes_and_removes_a_node_is_refused() {
        let existing = board_with_two();
        let b = board(serde_json::json!({
            "nodes": [{"name": "notes", "type": "ctx", "config": {"markdown": "changed"}}],
            "remove": {"nodes": ["notes"]}
        }));
        let refusals = validate(
            &b,
            &existing,
            ApplyPolicy {
                allow_patch: true,
                allow_delete: true,
                ..Default::default()
            },
        )
        .expect_err("one or the other");
        assert!(
            refusals
                .iter()
                .any(|r| matches!(r, Refusal::ConflictingChange { .. })),
            "{refusals:?}"
        );
    }

    #[test]
    fn a_wire_to_a_node_this_board_removes_is_refused() {
        let existing = board_with_two();
        let b = board(serde_json::json!({
            "nodes": [{"name": "brief", "type": "ctx", "config": {"markdown": "b"}}],
            "wires": [{"from": "brief", "to": "results", "type": "write"}],
            "remove": {"nodes": ["results"]}
        }));
        let refusals = validate(
            &b,
            &existing,
            ApplyPolicy {
                allow_delete: true,
                allow_wire: true,
                ..Default::default()
            },
        )
        .expect_err("wiring something on its way out");
        assert!(
            refusals
                .iter()
                .any(|r| matches!(r, Refusal::WireToRemovedNode { .. })),
            "{refusals:?}"
        );
    }

    /// A wire that a node deletion already takes must not be attempted twice: the engine cascades
    /// it, so a second call would fail and turn a clean apply into a reported partial.
    #[test]
    fn a_wire_that_the_node_deletion_takes_is_not_removed_twice() {
        let existing = board_with_two();
        let b = board(serde_json::json!({
            "nodes": [],
            "remove": {"nodes": ["results"],
                       "wires": [{"from": "notes", "to": "results", "type": "write"}]}
        }));
        let plan = validate(
            &b,
            &existing,
            ApplyPolicy {
                allow_delete: true,
                allow_unwire: true,
                ..Default::default()
            },
        )
        .expect("legal");
        assert!(plan.delete_wires.is_empty(), "{:?}", plan.delete_wires);
        assert_eq!(
            plan.delete_nodes[0].wires.len(),
            1,
            "it is shown under the node instead"
        );
    }

    // --- execution order ----------------------------------------------------

    async fn plan_with_removals() -> (Plan, ExistingBoard) {
        let existing = board_with_two();
        let b = board(serde_json::json!({
            "nodes": [{"name": "brief", "type": "ctx", "config": {"markdown": "b"}}],
            "remove": {"nodes": ["results"]}
        }));
        let plan = validate(
            &b,
            &existing,
            ApplyPolicy {
                allow_delete: true,
                allow_unwire: true,
                ..Default::default()
            },
        )
        .expect("legal");
        (plan, existing)
    }

    #[tokio::test]
    async fn a_removal_that_lands_is_reported_as_one() {
        let (plan, existing) = plan_with_removals().await;
        let client = FakeClient::new();
        let report = execute(&plan, &existing, &client).await;
        assert!(report.is_complete(), "{report:?}");
        assert_eq!(report.deleted_nodes, vec!["results".to_string()]);
        assert_eq!(client.deleted_nodes.lock().unwrap().len(), 1);
    }

    /// The property that makes removals safe to offer at all: if anything earlier failed, nothing
    /// is destroyed. A half-applied improve that ALSO deleted what it was replacing would leave
    /// the user with neither the old board nor the new one.
    #[tokio::test]
    async fn nothing_is_destroyed_when_an_earlier_step_failed() {
        let (plan, existing) = plan_with_removals().await;
        let mut client = FakeClient::new();
        client.fail_node = Some("brief".into());
        let report = execute(&plan, &existing, &client).await;

        assert!(!report.is_complete());
        assert!(
            report.deleted_nodes.is_empty(),
            "a node was destroyed anyway"
        );
        assert!(
            client.deleted_nodes.lock().unwrap().is_empty(),
            "the delete call was made despite an earlier failure"
        );
        let skipped = report
            .failures
            .iter()
            .find(|f| f.node.as_deref() == Some("results"))
            .expect("the deletion that did not happen is still reported");
        assert!(
            skipped.error.contains("nothing was destroyed"),
            "{skipped:?}"
        );
    }

    /// Capability comes off before any goes on, so a failure part-way leaves the board with less
    /// reach rather than more.
    #[tokio::test]
    async fn wires_are_removed_before_anything_is_added() {
        let existing = board_with_two();
        let b = board(serde_json::json!({
            "nodes": [{"name": "brief", "type": "ctx", "config": {"markdown": "b"}}],
            "remove": {"wires": [{"from": "notes", "to": "results", "type": "write"}]}
        }));
        let plan = validate(
            &b,
            &existing,
            ApplyPolicy {
                allow_unwire: true,
                ..Default::default()
            },
        )
        .expect("legal");
        let mut client = FakeClient::new();
        client.fail_node = Some("brief".into());
        let report = execute(&plan, &existing, &client).await;

        assert_eq!(
            report.deleted_wires.len(),
            1,
            "the unwire must already have happened when the create failed"
        );
        assert!(!report.is_complete());
    }

    #[test]
    fn a_board_that_cannot_be_read_is_refused_with_a_reason() {
        let refusal = crate::apply::read_board(serde_json::json!({"nodes": [{"name": 7}]}))
            .expect_err("a node with no type is not a board");
        assert!(
            matches!(refusal, Refusal::MalformedBoard { .. }),
            "{refusal:?}"
        );
        assert!(
            refusal.message().contains("could not be read"),
            "{}",
            refusal.message()
        );
    }

    /// Consent is bound to a plan, so the digest has to move when the plan does and stay put when
    /// it does not.
    #[test]
    fn the_digest_follows_the_plan() {
        let existing = board_with_two();
        let remove_one = board(serde_json::json!({"nodes": [], "remove": {"nodes": ["results"]}}));
        let policy = ApplyPolicy {
            allow_delete: true,
            allow_unwire: true,
            ..Default::default()
        };

        let first = validate(&remove_one, &existing, policy).unwrap().digest();
        let again = validate(&remove_one, &existing, policy).unwrap().digest();
        assert_eq!(first, again, "the same plan must have the same digest");

        let remove_other = board(serde_json::json!({"nodes": [], "remove": {"nodes": ["notes"]}}));
        let different = validate(&remove_other, &existing, policy).unwrap().digest();
        assert_ne!(
            first, different,
            "removing a different node is a different plan"
        );
    }
}
