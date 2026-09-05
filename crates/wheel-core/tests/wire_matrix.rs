//! Exhaustive test of the wire matrix: all 8 x 8 x 3 = 192 cells asserted
//! individually against ARCHITECTURE.md §3.
//!
//! The expected set below is transcribed from the table *by hand, one line per
//! documented cell*, deliberately NOT by calling `wire_allowed` — otherwise the
//! test would just be asserting the implementation against itself.

use wheel_core::{allowed_wires, check_wire, wire_allowed, NodeType as N, WireType as W};

/// Every (from, to, type) the contract says is ALLOWED. Everything else denied.
const EXPECTED_ALLOWED: &[(N, W, N)] = &[
    // agent →
    (N::Agent, W::Send, N::Agent),
    (N::Agent, W::Read, N::Ctx),
    (N::Agent, W::Write, N::Ctx),
    (N::Agent, W::Read, N::Table),
    (N::Agent, W::Write, N::Table),
    (N::Agent, W::Read, N::Vault),
    (N::Agent, W::Read, N::Chest),
    (N::Agent, W::Write, N::Chest),
    (N::Agent, W::Read, N::Script),
    (N::Agent, W::Read, N::Mcp),
    // ctx →
    (N::Ctx, W::Send, N::Agent),
    // endpoint →
    (N::Endpoint, W::Send, N::Agent),
    (N::Endpoint, W::Write, N::Table),
    (N::Endpoint, W::Send, N::Script),
    // script →
    (N::Script, W::Send, N::Agent),
    (N::Script, W::Read, N::Ctx),
    (N::Script, W::Write, N::Ctx),
    (N::Script, W::Read, N::Table),
    (N::Script, W::Write, N::Table),
    (N::Script, W::Read, N::Chest),
    (N::Script, W::Write, N::Chest),
    (N::Script, W::Read, N::Vault),
];

fn is_expected(from: N, to: N, ty: W) -> bool {
    EXPECTED_ALLOWED
        .iter()
        .any(|&(f, w, t)| f == from && t == to && w == ty)
}

#[test]
fn every_one_of_the_192_cells_matches_the_contract() {
    let mut checked = 0;
    for from in N::ALL {
        for to in N::ALL {
            for ty in W::ALL {
                checked += 1;
                assert_eq!(
                    wire_allowed(from, to, ty),
                    is_expected(from, to, ty),
                    "matrix mismatch for {from} --{ty}--> {to}"
                );
            }
        }
    }
    assert_eq!(checked, 192, "expected to cover 8*8*3 cells");
}

#[test]
fn allowed_count_is_exactly_the_documented_set() {
    assert_eq!(allowed_wires().len(), EXPECTED_ALLOWED.len());
    assert_eq!(EXPECTED_ALLOWED.len(), 22);
}

#[test]
fn container_types_have_no_outgoing_wires() {
    // "ctx, table, vault, chest, mcp have no other outgoing wires."
    for from in [N::Table, N::Vault, N::Chest, N::Mcp] {
        for to in N::ALL {
            for ty in W::ALL {
                assert!(
                    !wire_allowed(from, to, ty),
                    "{from} must have no outgoing wires, found {ty} -> {to}"
                );
            }
        }
    }
    // ctx's ONLY outgoing wire is send->agent.
    for to in N::ALL {
        for ty in W::ALL {
            let expect = to == N::Agent && ty == W::Send;
            assert_eq!(wire_allowed(N::Ctx, to, ty), expect);
        }
    }
}

#[test]
fn nothing_may_write_to_a_vault_or_an_agent() {
    for from in N::ALL {
        assert!(!wire_allowed(from, N::Vault, W::Write));
        assert!(!wire_allowed(from, N::Agent, W::Write));
        assert!(!wire_allowed(from, N::Agent, W::Read));
    }
}

#[test]
fn nothing_may_wire_into_an_endpoint() {
    for from in N::ALL {
        for ty in W::ALL {
            assert!(!wire_allowed(from, N::Endpoint, ty));
        }
    }
}

#[test]
fn write_implies_read_only_for_table_and_chest() {
    // Contract: "(`write` implies `read`)" appears on the table and chest rows.
    assert!(W::Write.satisfies(W::Read, N::Table));
    assert!(W::Write.satisfies(W::Read, N::Chest));
    // ...and nowhere else.
    for t in [N::Agent, N::Ctx, N::Endpoint, N::Script, N::Mcp, N::Vault] {
        assert!(
            !W::Write.satisfies(W::Read, t),
            "write must not imply read for {t}"
        );
    }
    // read never implies write, and send is unrelated to both.
    for t in N::ALL {
        assert!(!W::Read.satisfies(W::Write, t));
        assert!(!W::Send.satisfies(W::Read, t));
        assert!(!W::Read.satisfies(W::Send, t));
    }
    // Exact match always satisfies.
    for t in N::ALL {
        for w in W::ALL {
            assert!(w.satisfies(w, t));
        }
    }
}

#[test]
fn self_wires_are_rejected_even_when_the_pair_is_allowed() {
    let id = uuid::Uuid::new_v4();
    // agent->agent send is allowed as a pair, but not to yourself.
    let err = check_wire(id, N::Agent, id, N::Agent, W::Send).unwrap_err();
    assert!(matches!(err, wheel_core::WireError::SelfWire));

    let other = uuid::Uuid::new_v4();
    assert!(check_wire(id, N::Agent, other, N::Agent, W::Send).is_ok());
    assert!(check_wire(id, N::Agent, other, N::Vault, W::Write).is_err());
}
