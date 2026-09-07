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
use uuid::Uuid;
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

/// The most this step will attempt in one apply.
///
/// Not a security boundary — the authoritative per-project limit is engine-side — but a bound on
/// what one builder turn can ask for. Realising a board is one engine call per node and per wire,
/// so an unbounded board is an unbounded burst against a single project's engine, and the failure
/// would arrive as a slow partial apply rather than a refusal anyone can read.
pub const MAX_NODES: usize = 200;
pub const MAX_WIRES: usize = 1000;

/// Check an emitted board against the matrix and the current board.
///
/// Returns EVERY refusal, not the first: an LLM that got one wire wrong usually got several, and
/// handing them back one per round trip wastes the user's time.
pub fn validate(board: &EmittedBoard, existing: &ExistingBoard) -> Result<Plan, Vec<Refusal>> {
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

/// The board operations the apply step needs. Narrow on purpose: the `Orchestrator` trait is about
/// sandbox lifecycle, and a fake of four methods is what makes the executor testable without an
/// engine.
#[async_trait::async_trait]
pub trait BoardClient: Send + Sync {
    async fn create_node(&self, node: &EmittedNode) -> Result<Uuid, String>;
    async fn patch_config(&self, id: Uuid, config: &serde_json::Value) -> Result<(), String>;
    async fn add_wire(&self, from: Uuid, to: Uuid, wire_type: WireType) -> Result<(), String>;
}

/// One step that did not land, named so the report says which.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Failure {
    pub step: String,
    pub error: String,
}

/// Exactly what happened. Never "ok" for a partial apply — see `is_complete`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ApplyReport {
    pub created_nodes: Vec<String>,
    pub patched_nodes: Vec<String>,
    pub created_wires: Vec<String>,
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
            Err(error) => report.failures.push(Failure {
                step: format!("create node {:?}", node.name),
                error,
            }),
        }
    }

    for node in &plan.patch_nodes {
        let Some(id) = ids.get(&node.name).copied() else {
            report.failures.push(Failure {
                step: format!("patch node {:?}", node.name),
                error: "the node is no longer on the board".into(),
            });
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
            Err(error) => report.failures.push(Failure {
                step: format!("patch node {:?}", node.name),
                error,
            }),
        }
    }

    for wire in &plan.create_wires {
        let label = format!("{} -> {} ({})", wire.from, wire.to, wire.wire_type.as_str());
        let (Some(from), Some(to)) = (ids.get(&wire.from).copied(), ids.get(&wire.to).copied())
        else {
            let missing = if ids.contains_key(&wire.from) {
                &wire.to
            } else {
                &wire.from
            };
            report.failures.push(Failure {
                step: format!("create wire {label}"),
                error: format!("{missing:?} was not created, so this wire has no endpoint"),
            });
            continue;
        };
        match client.add_wire(from, to, wire.wire_type).await {
            Ok(()) => report.created_wires.push(label),
            Err(error) => report.failures.push(Failure {
                step: format!("create wire {label}"),
                error,
            }),
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
    }
    impl FakeClient {
        fn new() -> Self {
            Self {
                fail_node: None,
                fail_wire: false,
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
        let plan = validate(&b, &existing).expect("legal");
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
        let plan = validate(&b, &existing).expect("legal");
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
        let plan = validate(&b, &existing).expect("legal");
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
        let plan = validate(&b, &existing).expect("legal");
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
        let plan = validate(&b, &existing).expect("legal");
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

        let refusals = validate(&b, &existing).expect_err("a type change must be refused");
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

        let plan = validate(&b, &existing).expect("same type is legal");
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
        let refusals = validate(&b, &existing).expect_err("the collision must be refused first");
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

        let refusals = validate(&b, &ExistingBoard::default()).expect_err("too large");
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
        let plan = validate(&b, &ExistingBoard::default()).expect("exactly at the cap is legal");
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
        let refusals = validate(&b, &ExistingBoard::default()).expect_err("too many wires");
        assert!(
            matches!(refusals[0], Refusal::BoardTooLarge { .. }),
            "{:?}",
            refusals[0]
        );
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
