//! The capability layer: token → node → wire check.
//!
//! Every `/v1/cli/*` call resolves its authority here and nowhere else. That is
//! the point of putting it in one module with one entry point
//! ([`Caller::require`]): a node's wire set is its entire authority, so there
//! must be exactly one place that can be wrong, and it must be exhaustively
//! testable without HTTP.
//!
//! Default is DENY. A target that does not exist and a target the caller may
//! not reach are answered differently on purpose — exit 4 vs exit 3 — because
//! an agent needs to tell "I typo'd the name" from "I am not allowed".

use rusqlite::Connection;
use wheel_core::{Node, NodeType, WireType};

use crate::db::{board, tokens};

/// Why a capability check failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Denial {
    /// The presented token matches no live node. Its agent may have stopped
    /// (tokens rotate on start), or the token is fabricated.
    UnknownToken,
    /// The caller named something that is not a node on this board.
    NoSuchNode { name: String },
    /// The node exists and the caller may not do this to it.
    NoWire {
        from: String,
        to: String,
        required: WireType,
    },
}

impl Denial {
    /// Exit code the `wheel` CLI returns, mirroring `yoke` so agents already
    /// know what 3 and 4 mean.
    pub fn exit_code(&self) -> i32 {
        match self {
            Denial::NoSuchNode { .. } => wheel_core::EXIT_NOT_FOUND,
            _ => wheel_core::EXIT_WIRE_DENIED,
        }
    }

    /// The stable machine-readable code in the error body.
    pub fn code(&self) -> &'static str {
        match self {
            Denial::UnknownToken => "unauthorized",
            Denial::NoSuchNode { .. } => "not_found",
            Denial::NoWire { .. } => "wire_denied",
        }
    }
}

impl std::fmt::Display for Denial {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Denial::UnknownToken => write!(f, "unknown or expired node token"),
            Denial::NoSuchNode { name } => write!(f, "no such node: {name}"),
            Denial::NoWire { from, to, required } => {
                write!(f, "no wire from {from} to {to} (need: {required})")
            }
        }
    }
}

/// An authenticated caller: the node a presented token resolved to.
#[derive(Debug)]
pub struct Caller {
    pub node: Node,
}

impl Caller {
    /// Resolve a presented token to its node.
    pub fn authenticate(conn: &Connection, presented: &str) -> Result<Self, Denial> {
        let id = tokens::resolve(conn, presented)
            .ok()
            .flatten()
            .ok_or(Denial::UnknownToken)?;
        let node = board::get(conn, id)
            .ok()
            .flatten()
            .ok_or(Denial::UnknownToken)?;
        Ok(Self { node })
    }

    /// **The** capability check. Resolve `target` by name and confirm this
    /// caller holds a wire to it satisfying `required`.
    ///
    /// Returns the target node so callers cannot accidentally act on a
    /// different one than was checked — the check and the use are one step.
    pub fn require(
        &self,
        conn: &Connection,
        target: &str,
        required: WireType,
    ) -> Result<Node, Denial> {
        let to = board::get_by_name(conn, target)
            .ok()
            .flatten()
            .ok_or_else(|| Denial::NoSuchNode {
                name: target.to_string(),
            })?;

        if !self.node.has_wire(to.id, required, to.node_type()) {
            return Err(Denial::NoWire {
                from: self.node.name.to_string(),
                to: to.name.to_string(),
                required,
            });
        }
        Ok(to)
    }

    /// Every node this caller can reach, with the wire type, for `wheel ls`
    /// with no argument and `wheel connections` (§3c#7).
    pub fn reachable(&self, conn: &Connection) -> Vec<(Node, WireType)> {
        self.node
            .wires
            .iter()
            .filter_map(|w| {
                board::get(conn, w.to)
                    .ok()
                    .flatten()
                    .map(|n| (n, w.wire_type))
            })
            .collect()
    }

    /// Wires pointing AT this caller — the `←` half of `wheel connections`, so
    /// an agent can see who can reach it, not only what it can reach.
    pub fn inbound(&self, conn: &Connection) -> Vec<(Node, WireType)> {
        board::wires_to(conn, self.node.id)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(from, ty)| board::get(conn, from).ok().flatten().map(|n| (n, ty)))
            .collect()
    }
}

/// Is this node type allowed to use the CLI at all?
///
/// Only agents and scripts run code that could hold a token. A token belonging
/// to any other type would be a bug, and treating it as an ordinary denial
/// would hide that.
pub fn may_use_cli(t: NodeType) -> bool {
    matches!(t, NodeType::Agent | NodeType::Script)
}

/// Resolve a `<node>/<row>` address into its parts.
///
/// Split on the FIRST slash only: chest keys are paths and legitimately contain
/// slashes, so `files/a/b/c.txt` is the blob `a/b/c.txt` in the chest `files`.
pub fn split_address(addr: &str) -> (&str, Option<&str>) {
    match addr.split_once('/') {
        Some((node, rest)) if !rest.is_empty() => (node, Some(rest)),
        // A trailing slash addresses the NODE, so the slash must be dropped —
        // returning it would make the node name "notes/", which resolves to
        // nothing and reports a confusing "no such node".
        Some((node, _)) => (node, None),
        None => (addr, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;
    use wheel_core::{AgentConfig, ChestConfig, CtxConfig, NodeConfig, NodeName, Position};

    fn node(name: &str, config: NodeConfig) -> Node {
        Node::new(
            Uuid::new_v4(),
            NodeName::new(name).unwrap(),
            Position::default(),
            config,
        )
    }
    fn agent(name: &str) -> Node {
        node(name, NodeConfig::Agent(AgentConfig::default()))
    }
    fn ctx(name: &str) -> Node {
        node(
            name,
            NodeConfig::Ctx(CtxConfig {
                markdown: "m".into(),
            }),
        )
    }

    /// A board with `caller` wired read-only to `notes`, send to `peer`.
    fn fixture() -> (Connection, Node, Node, Node) {
        let c = crate::db::open_memory().unwrap();
        let caller = agent("caller");
        let peer = agent("peer");
        let notes = ctx("notes");
        for n in [&caller, &peer, &notes] {
            board::create(&c, n).unwrap();
        }
        board::add_wire(&c, caller.id, notes.id, WireType::Read, None).unwrap();
        board::add_wire(&c, caller.id, peer.id, WireType::Send, None).unwrap();
        let caller = board::get(&c, caller.id).unwrap().unwrap();
        (c, caller, peer, notes)
    }

    #[test]
    fn a_token_authenticates_as_exactly_its_own_node() {
        let (c, caller, peer, _) = fixture();
        let t = tokens::mint(&c, caller.id).unwrap();
        let who = Caller::authenticate(&c, &t.plaintext).unwrap();
        assert_eq!(who.node.id, caller.id);
        assert_ne!(who.node.id, peer.id);
    }

    #[test]
    fn a_fabricated_or_expired_token_authenticates_as_nobody() {
        let (c, caller, _, _) = fixture();
        let t = tokens::mint(&c, caller.id).unwrap();
        assert!(Caller::authenticate(&c, "deadbeef").is_err());
        assert_eq!(
            Caller::authenticate(&c, "deadbeef").unwrap_err(),
            Denial::UnknownToken
        );
        // Rotation invalidates: a token kept from a previous run is nobody.
        tokens::mint(&c, caller.id).unwrap();
        assert_eq!(
            Caller::authenticate(&c, &t.plaintext).unwrap_err(),
            Denial::UnknownToken
        );
    }

    #[test]
    fn a_held_wire_is_allowed_and_returns_the_checked_target() {
        let (c, caller, _, notes) = fixture();
        let t = tokens::mint(&c, caller.id).unwrap();
        let who = Caller::authenticate(&c, &t.plaintext).unwrap();

        let got = who.require(&c, "notes", WireType::Read).unwrap();
        assert_eq!(got.id, notes.id, "the check must return what it checked");
    }

    #[test]
    fn a_wire_the_caller_does_not_hold_is_denied_with_exit_3() {
        let (c, caller, _, _) = fixture();
        let t = tokens::mint(&c, caller.id).unwrap();
        let who = Caller::authenticate(&c, &t.plaintext).unwrap();

        // Read-only to notes: writing is denied.
        let err = who.require(&c, "notes", WireType::Write).unwrap_err();
        assert_eq!(err.exit_code(), 3);
        assert_eq!(err.code(), "wire_denied");
        assert_eq!(
            err.to_string(),
            "no wire from caller to notes (need: write)"
        );
    }

    /// Exit 4 vs exit 3 is a real distinction: an agent must be able to tell a
    /// typo from a permission problem.
    #[test]
    fn a_missing_node_is_exit_4_not_a_denial() {
        let (c, caller, _, _) = fixture();
        let t = tokens::mint(&c, caller.id).unwrap();
        let who = Caller::authenticate(&c, &t.plaintext).unwrap();

        let err = who.require(&c, "nonexistent", WireType::Read).unwrap_err();
        assert_eq!(err.exit_code(), 4);
        assert_eq!(err.code(), "not_found");
        assert!(err.to_string().contains("no such node: nonexistent"));
    }

    /// The wrong wire TYPE is still a denial: holding `send` to a peer does not
    /// let you read it.
    #[test]
    fn the_wrong_wire_type_does_not_satisfy_a_different_one() {
        let (c, caller, _, _) = fixture();
        let t = tokens::mint(&c, caller.id).unwrap();
        let who = Caller::authenticate(&c, &t.plaintext).unwrap();

        assert!(who.require(&c, "peer", WireType::Send).is_ok());
        for wrong in [WireType::Read, WireType::Write] {
            assert!(
                who.require(&c, "peer", wrong).is_err(),
                "send must not satisfy {wrong}"
            );
        }
    }

    /// write ⊃ read for table and chest only (§3 matrix).
    #[test]
    fn a_write_wire_grants_read_on_a_chest_but_not_on_a_ctx() {
        let c = crate::db::open_memory().unwrap();
        let a = agent("a");
        let files = node("files", NodeConfig::Chest(ChestConfig {}));
        let notes = ctx("notes");
        for n in [&a, &files, &notes] {
            board::create(&c, n).unwrap();
        }
        board::add_wire(&c, a.id, files.id, WireType::Write, None).unwrap();
        board::add_wire(&c, a.id, notes.id, WireType::Write, None).unwrap();
        let t = tokens::mint(&c, a.id).unwrap();
        let who = Caller::authenticate(&c, &t.plaintext).unwrap();

        assert!(
            who.require(&c, "files", WireType::Read).is_ok(),
            "write implies read on a chest"
        );
        assert!(
            who.require(&c, "notes", WireType::Read).is_err(),
            "write must NOT imply read on a ctx: a write-only ctx wire is a \
             meaningful capability to hand an agent"
        );
    }

    #[test]
    fn reachable_lists_what_i_can_touch_and_inbound_lists_who_can_touch_me() {
        let (c, caller, peer, notes) = fixture();
        board::add_wire(&c, notes.id, caller.id, WireType::Send, None).unwrap();
        let caller = board::get(&c, caller.id).unwrap().unwrap();
        let who = Caller { node: caller };

        let mut out: Vec<_> = who
            .reachable(&c)
            .into_iter()
            .map(|(n, w)| (n.name.to_string(), w))
            .collect();
        out.sort();
        assert_eq!(
            out,
            vec![
                ("notes".to_string(), WireType::Read),
                ("peer".to_string(), WireType::Send)
            ]
        );
        assert_eq!(peer.name.as_str(), "peer");

        let inbound: Vec<_> = who
            .inbound(&c)
            .into_iter()
            .map(|(n, w)| (n.name.to_string(), w))
            .collect();
        assert_eq!(inbound, vec![("notes".to_string(), WireType::Send)]);
    }

    #[test]
    fn only_agents_and_scripts_may_hold_a_cli_token() {
        for t in NodeType::ALL {
            let expect = matches!(t, NodeType::Agent | NodeType::Script);
            assert_eq!(may_use_cli(t), expect, "may_use_cli({t})");
        }
    }

    /// Chest keys are paths, so only the FIRST slash separates node from row.
    #[test]
    fn an_address_splits_on_the_first_slash_only() {
        assert_eq!(split_address("notes"), ("notes", None));
        assert_eq!(split_address("table/row1"), ("table", Some("row1")));
        assert_eq!(
            split_address("files/a/b/c.txt"),
            ("files", Some("a/b/c.txt"))
        );
        // A trailing slash addresses the node, not an empty row.
        assert_eq!(split_address("notes/"), ("notes", None));
    }
}
