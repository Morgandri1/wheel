// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

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
//! exists. It does not cover engine-side failures — a name collision, or anything else the engine
//! decides at creation time — which is why the apply result reports what landed rather than
//! promising atomicity we cannot deliver here.
//!
//! **There is no per-project node cap at any layer today.** An earlier version of this comment said
//! this step "does not cover per-project caps", which implied a backstop that does not exist:
//! nothing in the engine counts a project's nodes, and §3e's default-50 is unimplemented and queued
//! with SDK. `MAX_NODES`/`MAX_WIRES` below bound ONE REQUEST, not a project total, so a caller can
//! still grow a board without limit an apply at a time. Corrected because a comment promising a
//! guard that is not there is worse than no comment — it tells the next reader to stop looking.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;
use wheel_core::{
    validate_config_with, validate_name, validate_table_name, wire_allowed, NodeConfig, NodeType,
    Position, WireType,
};

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
    /// An emitted node reuses the name of an existing node of a DIFFERENT type.
    ///
    /// Refused rather than resolved. Treating it as a patch was silently false — the engine rejects
    /// a type change, so the apply died mid-way with a confusing partial board; and either
    /// precedence rule (emitted wins / existing wins) silently validates the board's wires against
    /// a type one side does not agree with. There is no answer here that is not a guess about what
    /// the builder meant, so the caller is told.
    NodeTypeMismatch {
        name: String,
        existing_type: NodeType,
        emitted_type: NodeType,
    },
    /// The board names a node that already exists, and this apply may only create.
    ///
    /// The default is create-only on purpose. A builder-emitted board that merely MENTIONS an
    /// existing node would otherwise modify it, and "the LLM named it" is not consent. Patching is
    /// opt-in per apply, and the refusal names every node it would have touched so the caller can
    /// show the user exactly what they are being asked to allow.
    PatchNotPermitted { name: String },
    /// The wire attaches to a node that already exists, and this apply may not rewire.
    ///
    /// A WIRE IS THE CAPABILITY. Wiring an existing node changes what it can do — or what can reach
    /// it — without touching a byte of its config, so create-only protected the wrong half until
    /// ADVERSARY 050. `ctx -> agent (send)` injects into that agent's prompt permanently;
    /// `agent -> vault (read)` hands it secrets it did not have; an `auth:none` endpoint wired to an
    /// agent puts the public internet on its inbox. None of those edit the node.
    ///
    /// Both directions count: an inbound wire adds a channel into the node, an outbound one grants
    /// it a new reach. Only a wire between two nodes this same board is CREATING is consent-free,
    /// because nothing pre-existing is being changed.
    WireTouchesExistingNode {
        from: String,
        to: String,
        wire_type: WireType,
        /// The endpoint(s) that already exist — what the user is being asked to allow.
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
    /// A node's name fails the §3 name contract — charset, length, a reserved word, or, for a
    /// `table` node specifically, the stricter table-name rule (it becomes `t_<name>` in sqlite).
    ///
    /// The engine enforces this on create; before now this step did not, so a bad name passed
    /// pre-validation and only failed live, at the actual engine call — the exact
    /// "looks-validated-but-isn't" gap this module otherwise exists to close.
    InvalidName { name: String, reason: String },
    /// A node's config fails `wheel_core::validate_config_with` — an SSRF-denied tool `base_url`,
    /// an over-long agent system prompt, a malformed vault key, and so on. Same gap as
    /// `InvalidName`: the engine already refuses these at create, this step did not until now.
    InvalidConfig { name: String, reason: String },
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
            Refusal::NodeTypeMismatch { name, existing_type, emitted_type } => format!(
                "{name:?} is already on the board as a {}, and the board defines it as a {}; \
                 rename one of them — a node's type cannot be changed",
                existing_type.as_str(),
                emitted_type.as_str()
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

/// A node already on the board, as the apply step needs to see it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExistingNode {
    pub id: Uuid,
    pub node_type: NodeType,
}

/// What is already on the board, as the apply step needs to see it.
#[derive(Debug, Clone, Default)]
pub struct ExistingBoard {
    /// name -> id and type. The id is why this is not just a type map: wires are created by id, and
    /// an existing node is patched by id.
    pub nodes: HashMap<String, ExistingNode>,
    /// Wires already present, so applying the same board twice adds nothing.
    pub wires: Vec<(String, String, WireType)>,
}

/// What an apply is permitted to do, beyond being legal.
///
/// Separate from validity: a board can be perfectly well-formed and still ask for more authority
/// than the caller granted. Defaults to the conservative answer, so a caller that forgets to think
/// about it gets create-only rather than "modify whatever this board happens to name".
#[derive(Debug, Clone, Copy, Default)]
pub struct ApplyPolicy {
    /// Allow the board to modify the CONFIG of nodes that already exist. Off by default.
    pub allow_patch: bool,
    /// Allow the board to WIRE nodes that already exist. Off by default.
    ///
    /// Deliberately separate from `allow_patch` rather than folded into it. They are different
    /// consents over different risks — "you may rewrite this agent's prompt" is not "you may put a
    /// public endpoint on its inbox" — and a user granting one should not silently grant the other.
    /// It also lets the confirm step name them apart: these nodes will be MODIFIED, these existing
    /// nodes will be WIRED.
    pub allow_wire: bool,
}

/// The most this step will attempt in one apply.
///
/// A bound on ONE REQUEST, and — measured, not assumed — the only bound that exists anywhere.
///
/// This used to claim "the authoritative per-project limit is engine-side". There is no such limit:
/// SDK and ADVERSARY both grepped and nothing counts a project's nodes, so §3e's default-50 is
/// documented rather than implemented. That makes these numbers load-bearing in a way they were not
/// written to be, and it is worth being plain about what they do NOT do: they cap a single apply,
/// so a caller may still grow a board indefinitely one apply at a time.
///
/// What they do buy: realising a board is one engine call per node and per wire, so an unbounded
/// board is an unbounded burst at a single project's engine, arriving as a slow partial apply
/// rather than a refusal anyone can read.
///
/// The values are a judgement — generous but bounded — not a measurement.
pub const MAX_NODES: usize = 200;
pub const MAX_WIRES: usize = 1000;

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
    // board would otherwise produce thousands of refusals nobody can read.
    if board.nodes.len() > MAX_NODES || board.wires.len() > MAX_WIRES {
        return Err(vec![Refusal::BoardTooLarge {
            nodes: board.nodes.len(),
            wires: board.wires.len(),
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
    // Name and config, per node. Same checks the engine runs on create (§3), run here first so a
    // bad name or an SSRF-denied tool base_url is refused before anything exists rather than only
    // failing live at the engine — the CI gate that validates template files offline
    // (docs/proposals/wow-templates.md §3) depends on this to be authoritative.
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
        // allowlist (`validate_config_with`'s own doc), so this is the same answer the live
        // create call would give, not an approximation of it.
        if let Err(e) = validate_config_with(&node.config, &[]) {
            refusals.push(Refusal::InvalidConfig {
                name: node.name.clone(),
                reason: e.to_string(),
            });
        }
    }

    // A name on both sides must agree. Refused above as a guess nobody can justify; and because it
    // is refused, the resolution order below cannot matter — existing wins, which is the
    // conservative half of ADVERSARY 049 and makes the invariant obvious rather than incidental.
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
            if !policy.allow_patch {
                refusals.push(Refusal::PatchNotPermitted {
                    name: node.name.clone(),
                });
                continue;
            }
            patch_nodes.push(node.clone());
        } else {
            create_nodes.push(node.clone());
        }
    }

    if !refusals.is_empty() {
        return Err(refusals);
    }

    let create_wires: Vec<EmittedWire> = board
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

    // ADVERSARY 050: a wire is the capability. Attaching one to a node that already exists changes
    // what that node can do, or what can reach it, without editing its config — so it needs the
    // same consent that editing it does. Only a wire between two nodes THIS board is creating is
    // consent-free, because nothing pre-existing is altered.
    //
    // Checked after the duplicate filter on purpose: re-applying an unchanged board must stay a
    // no-op rather than demanding consent for wires that are already there.
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
    })
}

/// The board operations the apply step needs. Narrow on purpose: the `Orchestrator` trait is about
/// sandbox lifecycle, and a fake of four methods is what makes the executor testable without an
/// engine.
#[async_trait::async_trait]
pub trait BoardClient: Send + Sync {
    async fn create_node(&self, node: &EmittedNode) -> Result<Uuid, String>;
    async fn patch_config(&self, id: Uuid, config: &serde_json::Value) -> Result<(), String>;
    /// `Ok(Some(warning))` is a wire the engine created but flagged as deserving the operator's
    /// attention (the same non-error, board-state-flag mechanism `wheel-engine`'s own `add_wire`
    /// uses — e.g. an ambiguous vault credential, or an `endpoint(auth:none)->agent(send)` wire
    /// whose target agent also holds a `ctx` write wire, finding 043's addendum). The wire is
    /// created either way; this is purely advisory.
    async fn add_wire(
        &self,
        from: Uuid,
        to: Uuid,
        wire_type: WireType,
    ) -> Result<Option<String>, String>;
}

/// A wire, as a consumer needs it: addressable, not a sentence.
///
/// These are rendered on a canvas and highlighted when they fail, so the shape is structured and
/// the caller formats. An earlier version returned "notes -> researcher (send)" — readable in a log
/// and useless to a UI that has to find the edge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WireRef {
    pub from: String,
    pub to: String,
    #[serde(rename = "type")]
    pub wire_type: WireType,
}

impl WireRef {
    /// From an emitted wire, for a preview built outside this module.
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
    /// The human form, for a `step` label.
    fn label(&self) -> String {
        format!("{} -> {} ({})", self.from, self.to, self.wire_type.as_str())
    }
}

/// One step that did not land, named so the report says which.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Failure {
    pub step: String,
    pub error: String,
    /// The node this step was about, when it was about one — so a UI can mark it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    /// The wire this step was about, when it was about one.
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

    /// A step this module does not itself run, reported through the same shape rather than a
    /// second one — `routes::instantiate` uses these for its capability-patch and sandbox-liveness
    /// steps, so one failure list, one rendering path, whatever stage a failure came from.
    pub fn step(step: impl Into<String>, error: impl Into<String>) -> Self {
        Self {
            step: step.into(),
            error: error.into(),
            node: None,
            wire: None,
        }
    }

    /// The sandbox never became reachable, so attempting an apply against it would only produce a
    /// confusing `engine_unreachable` rather than a useful failure.
    pub fn sandbox_did_not_start() -> Self {
        Self::step("sandbox", "the project engine did not become healthy")
    }

    /// The capability patch (step 4 of the instantiate sequence) failed.
    pub fn capabilities(error: impl Into<String>) -> Self {
        Self::step("capabilities", error)
    }
}

/// A wire that was created but that the engine flagged as deserving the operator's attention —
/// not a refusal (the wire exists), not silently dropped either. Board-apply's own leg of the
/// same mechanism `wheel-engine`'s `board_routes.rs` already surfaces to the interactive UI: a
/// caller building a board through this route deserves the identical warning, not a discarded
/// response body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WireWarning {
    pub wire: WireRef,
    pub message: String,
}

/// Exactly what happened. Never "ok" for a partial apply — see `is_complete`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ApplyReport {
    pub created_nodes: Vec<String>,
    pub patched_nodes: Vec<String>,
    pub created_wires: Vec<WireRef>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<WireWarning>,
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
/// Nodes first, then wires, because a wire needs both endpoints to exist. A node that fails to
/// create takes its wires with it — they are recorded as failures naming the missing endpoint
/// rather than attempted and rejected by the engine, so the report says why rather than echoing a
/// 404 the user cannot interpret.
///
/// This is NOT atomic and does not pretend to be: there is no batch route and no transaction across
/// these calls (see `docs/proposals/apply-step-constraints.md`). What it guarantees is that nothing
/// ILLEGAL was attempted — `validate` ran first — and that the report names every step that failed.
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
        let Some(id) = ids.get(&node.name).copied() else {
            report.failures.push(Failure::node(
                &node.name,
                format!("patch node {:?}", node.name),
                "the node is no longer on the board".into(),
            ));
            continue;
        };
        // Only `config` is sent, so the merge leaves name and position alone; and because the
        // engine merges rather than replaces, fields this board never mentioned keep their values.
        let config = serde_json::json!({ "config": serde_json::to_value(&node.config)
            .ok()
            .and_then(|v| v.get("config").cloned())
            .unwrap_or(serde_json::Value::Null) });
        match client.patch_config(id, &config).await {
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
            Ok(warning) => {
                report.created_wires.push(reference.clone());
                if let Some(message) = warning {
                    report.warnings.push(WireWarning {
                        wire: reference,
                        message,
                    });
                }
            }
            Err(error) => report.failures.push(Failure::wire(&reference, error)),
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
        wire_warning: Option<String>,
        created: std::sync::Mutex<Vec<String>>,
        patched: std::sync::Mutex<Vec<Uuid>>,
    }
    impl FakeClient {
        fn new() -> Self {
            Self {
                fail_node: None,
                fail_wire: false,
                wire_warning: None,
                created: std::sync::Mutex::new(Vec::new()),
                patched: std::sync::Mutex::new(Vec::new()),
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
        async fn patch_config(&self, id: Uuid, _c: &serde_json::Value) -> Result<(), String> {
            self.patched.lock().unwrap().push(id);
            Ok(())
        }
        async fn add_wire(
            &self,
            _f: Uuid,
            _t: Uuid,
            _w: WireType,
        ) -> Result<Option<String>, String> {
            if self.fail_wire {
                return Err("engine refused the wire".into());
            }
            Ok(self.wire_warning.clone())
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
            },
        )
        .expect("legal");
        let report = execute(&plan, &existing, &FakeClient::new()).await;

        assert!(report.is_complete(), "{report:?}");
        assert_eq!(report.created_nodes.len(), 2);
        assert_eq!(report.created_wires.len(), 1);
        assert!(report.failures.is_empty());
        assert!(report.warnings.is_empty());
    }

    /// A wire the engine creates but flags (the same non-error mechanism `board_routes.rs`
    /// surfaces to the interactive UI) must not be silently dropped just because this board came
    /// through the apply route rather than a hand-drawn wire — it is still `is_complete()` (the
    /// wire was NOT refused), but the caller can see exactly which wire and why.
    #[tokio::test]
    async fn a_flagged_wire_is_still_created_and_the_warning_is_reported_against_it() {
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
            },
        )
        .expect("legal");
        let mut client = FakeClient::new();
        client.wire_warning = Some("exposes notes to unauthenticated input".into());
        let report = execute(&plan, &existing, &client).await;

        assert!(
            report.is_complete(),
            "a flagged wire is not a failure: {report:?}"
        );
        assert_eq!(report.created_wires.len(), 1);
        assert_eq!(report.warnings.len(), 1);
        assert_eq!(report.warnings[0].wire, report.created_wires[0]);
        assert_eq!(
            report.warnings[0].message,
            "exposes notes to unauthenticated input"
        );
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
            ExistingNode {
                id: researcher,
                node_type: NodeType::Agent,
            },
        );
        existing.nodes.insert(
            "untouched".into(),
            ExistingNode {
                id: Uuid::new_v4(),
                node_type: NodeType::Ctx,
            },
        );

        let b = board(serde_json::json!({"nodes": [agent("researcher")], "wires": []}));
        let plan = validate(
            &b,
            &existing,
            ApplyPolicy {
                allow_patch: true,
                allow_wire: true,
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
            ExistingNode {
                id: Uuid::new_v4(),
                node_type: NodeType::Agent,
            },
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
            ExistingNode {
                id: Uuid::new_v4(),
                node_type: NodeType::Agent,
            },
        );
        let b = board(serde_json::json!({"nodes": [agent("researcher")], "wires": []}));

        let plan = validate(
            &b,
            &existing,
            ApplyPolicy {
                allow_patch: true,
                allow_wire: true,
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
            ExistingNode {
                id: Uuid::new_v4(),
                node_type: NodeType::Vault,
            },
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
            ExistingNode {
                id: Uuid::new_v4(),
                node_type: NodeType::Agent,
            },
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
                ExistingNode {
                    id: Uuid::new_v4(),
                    node_type: NodeType::Agent,
                },
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
            ExistingNode {
                id: Uuid::new_v4(),
                node_type: NodeType::Agent,
            },
        );
        let b = board(serde_json::json!({"nodes": [agent("researcher")], "wires": []}));

        let plan = validate(
            &b,
            &existing,
            ApplyPolicy {
                allow_patch: true,
                allow_wire: true,
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
            ExistingNode {
                id: Uuid::new_v4(),
                node_type: NodeType::Agent,
            },
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
            ExistingNode {
                id: Uuid::new_v4(),
                node_type: NodeType::Agent,
            },
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
            ExistingNode {
                id: Uuid::new_v4(),
                node_type: NodeType::Agent,
            },
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
            ExistingNode {
                id: Uuid::new_v4(),
                node_type: NodeType::Agent,
            },
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
            ExistingNode {
                id: Uuid::new_v4(),
                node_type: NodeType::Ctx,
            },
        );
        existing.nodes.insert(
            "pm".into(),
            ExistingNode {
                id: Uuid::new_v4(),
                node_type: NodeType::Agent,
            },
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
            ExistingNode {
                id: Uuid::new_v4(),
                node_type: NodeType::Agent,
            },
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
            ExistingNode {
                id: Uuid::new_v4(),
                node_type: NodeType::Agent,
            },
        );
        existing.nodes.insert(
            "untouched".into(),
            ExistingNode {
                id: Uuid::new_v4(),
                node_type: NodeType::Ctx,
            },
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
            },
        )
        .expect("legal");

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
        existing.nodes.insert(
            "notes".into(),
            ExistingNode {
                id: Uuid::new_v4(),
                node_type: NodeType::Ctx,
            },
        );
        existing.nodes.insert(
            "researcher".into(),
            ExistingNode {
                id: Uuid::new_v4(),
                node_type: NodeType::Agent,
            },
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
}
