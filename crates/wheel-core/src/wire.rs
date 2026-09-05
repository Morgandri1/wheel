//! Wires, and the wire matrix.
//!
//! A node's wire set **is** its capability set: the engine authorises every
//! `/v1/cli/*` call by resolving the caller's token to a node and checking that
//! a wire of the required type exists to the target. This module is the single
//! source of truth for which wires may exist at all; the engine and the API
//! both call [`wire_allowed`] at creation time, and the web UI mirrors it
//! client-side from the exported JSON schema.
//!
//! Default is DENY: [`wire_allowed`] enumerates the allowed triples and
//! everything else is rejected.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::node::NodeType;

/// What a wire permits. Wires are directional and stored on the *source* node.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum WireType {
    /// Read the target's data (`wheel read`, `table query`, `chest get|ls`,
    /// `secret get`, `run <script>`, MCP attachment).
    Read,
    /// Mutate the target's data. For `table` and `chest`, write **implies**
    /// read — see [`WireType::satisfies`].
    Write,
    /// Deliver a message to the target (agent→agent, endpoint→agent,
    /// script→agent) or, for `ctx`→`agent`, inject context into its prompt.
    Send,
}

impl WireType {
    pub const ALL: [WireType; 3] = [WireType::Read, WireType::Write, WireType::Send];

    pub fn as_str(self) -> &'static str {
        match self {
            WireType::Read => "read",
            WireType::Write => "write",
            WireType::Send => "send",
        }
    }

    /// Does a wire of type `self` satisfy a requirement for `required`?
    ///
    /// Exact match always does. Additionally, per the §3 matrix, `write`
    /// implies `read` on the node types where the table says so
    /// ("`write` implies `read`" appears on the table and chest rows), so an
    /// agent wired `write` to a table may also `SELECT` from it without a
    /// second wire.
    ///
    /// Note this is deliberately *not* universal: a `write` wire to a `ctx`
    /// node does not grant `read`, because ARCHITECTURE.md lists those as two
    /// independent cells and a write-only ctx wire is a meaningful thing to
    /// hand an agent (it can append notes it is not allowed to read back).
    pub fn satisfies(self, required: WireType, target: NodeType) -> bool {
        if self == required {
            return true;
        }
        matches!(
            (self, required, target),
            (
                WireType::Write,
                WireType::Read,
                NodeType::Table | NodeType::Chest
            )
        )
    }
}

impl std::fmt::Display for WireType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// An outgoing wire, as stored on its source node (`Node::wires`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Wire {
    /// Target node id.
    pub to: Uuid,
    #[serde(rename = "type")]
    pub wire_type: WireType,
}

impl Wire {
    pub fn new(to: Uuid, wire_type: WireType) -> Self {
        Self { to, wire_type }
    }
}

/// A wire including its source, used by API/engine payloads
/// (`POST /v1/wires {from,to,type}`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WireSpec {
    pub from: Uuid,
    pub to: Uuid,
    #[serde(rename = "type")]
    pub wire_type: WireType,
}

/// **The wire matrix.** `true` iff a wire `from -> to` of type `ty` may exist.
///
/// This is a pure function of the three node/wire types — no board state, no
/// I/O — so it can be tested exhaustively over all 8×8×3 = 192 cells and
/// mirrored exactly in the UI. Anything not listed here is denied.
///
/// Transcribed from ARCHITECTURE.md §3 "Wire semantics matrix".
pub const fn wire_allowed(from: NodeType, to: NodeType, ty: WireType) -> bool {
    use NodeType as N;
    use WireType as W;

    match (from, to, ty) {
        // --- agent outgoing -------------------------------------------------
        // `wheel msg <agent>` delivers into the target agent's inbox.
        (N::Agent, N::Agent, W::Send) => true,
        // `wheel read <ctx>` / `wheel write <ctx> --file f.md`
        (N::Agent, N::Ctx, W::Read | W::Write) => true,
        // `wheel table query` (read) + INSERT/UPDATE/DELETE (write)
        (N::Agent, N::Table, W::Read | W::Write) => true,
        // keys exported as env at spawn + `wheel secret get`. Never writable.
        (N::Agent, N::Vault, W::Read) => true,
        // `wheel chest get|ls` (read) + `put|rm` (write)
        (N::Agent, N::Chest, W::Read | W::Write) => true,
        // `wheel run <script> [args...]`
        (N::Agent, N::Script, W::Read) => true,
        // MCP server attached to the harness config at next start
        (N::Agent, N::Mcp, W::Read) => true,

        // MCP-style tool operations are attached to the agent at next start.
        (N::Agent, N::Tool, W::Read) => true,

        // --- tool outgoing --------------------------------------------------
        // A `vault` fill on an operation resolves at call time; the tool node
        // needs its own wire for it (§3d), so the vault dependency is visible
        // on the board rather than hidden inside an operation's config.
        (N::Tool, N::Vault, W::Read) => true,

        // --- ctx outgoing ---------------------------------------------------
        // INJECTION: ctx markdown is prepended to the agent's prompt on start
        // and after every context clear. ctx has no other outgoing wires.
        (N::Ctx, N::Agent, W::Send) => true,

        // --- endpoint outgoing ----------------------------------------------
        // each HTTP hit delivered as a message
        (N::Endpoint, N::Agent, W::Send) => true,
        // JSON body inserted as a row
        (N::Endpoint, N::Table, W::Write) => true,
        // script invoked with the request; response_mode:script returns stdout
        (N::Endpoint, N::Script, W::Send) => true,
        // Resolve the endpoint's `auth.vault_ref` bearer secret (§3). Without
        // this row `auth: {mode: "bearer"}` is unimplementable.
        (N::Endpoint, N::Vault, W::Read) => true,

        // --- script outgoing ------------------------------------------------
        // `wheel msg` from inside the script (scoped to ITS wires)
        (N::Script, N::Agent, W::Send) => true,
        // "same as agent" for ctx/table/chest/vault
        (N::Script, N::Ctx, W::Read | W::Write) => true,
        (N::Script, N::Table, W::Read | W::Write) => true,
        (N::Script, N::Chest, W::Read | W::Write) => true,
        (N::Script, N::Vault, W::Read) => true,
        // "script → tool | same as agent" (§3).
        (N::Script, N::Tool, W::Read) => true,

        // --- everything else is denied --------------------------------------
        // In particular: table, vault, chest and mcp have NO outgoing wires;
        // nothing may write to a vault or to an agent; no node may wire to an
        // endpoint; a tool's only outgoing wire is read->vault.
        _ => false,
    }
}

/// Every allowed `(from, to, type)` triple, derived from [`wire_allowed`].
/// Useful for docs, tests and the UI legend.
pub fn allowed_wires() -> Vec<(NodeType, NodeType, WireType)> {
    let mut out = Vec::new();
    for from in NodeType::ALL {
        for to in NodeType::ALL {
            for ty in WireType::ALL {
                if wire_allowed(from, to, ty) {
                    out.push((from, to, ty));
                }
            }
        }
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WireError {
    #[error("a {from} node may not have a {wire_type} wire to a {to} node")]
    NotAllowed {
        from: NodeType,
        to: NodeType,
        wire_type: WireType,
    },
    #[error("a node may not be wired to itself")]
    SelfWire,
}

/// Validate a proposed wire: rejects self-wires and anything the matrix denies.
pub fn check_wire(
    from_id: Uuid,
    from: NodeType,
    to_id: Uuid,
    to: NodeType,
    wire_type: WireType,
) -> Result<(), WireError> {
    if from_id == to_id {
        return Err(WireError::SelfWire);
    }
    if !wire_allowed(from, to, wire_type) {
        return Err(WireError::NotAllowed {
            from,
            to,
            wire_type,
        });
    }
    Ok(())
}
