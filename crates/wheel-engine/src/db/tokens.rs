//! Per-node capability tokens.
//!
//! A token IS a node's authority: whatever presents it is treated as that node
//! and gets exactly that node's wires. Three consequences shape this module:
//!
//! * Only the SHA-256 is stored, so reading the database yields nothing usable.
//! * Tokens are rotated on every start, so a token recovered from a stopped
//!   agent's filesystem is already dead.
//! * The token reaches the child as a 0600 FILE, never an environment variable
//!   (ADVERSARY F007): `/proc/<pid>/environ` is readable by the same uid, so an
//!   env token would hand every co-resident child every sibling's authority.

use anyhow::{Context, Result};
use rand::RngCore;
use rusqlite::{params, Connection, OptionalExtension};
use uuid::Uuid;
use wheel_core::{sha256_hex, Timestamp};

/// 32 bytes, hex-encoded. Long enough that guessing is not a strategy.
const TOKEN_BYTES: usize = 32;

/// A freshly minted token. The plaintext exists only here and in the 0600 file
/// handed to the child; it is never stored and never logged.
pub struct MintedToken {
    pub plaintext: String,
    pub node_id: Uuid,
}

/// Mint a token for `node`, replacing any it already had.
///
/// Rotation on start is deliberate: it bounds the lifetime of a leaked token to
/// one agent run, and means a token found on disk after a stop is inert.
pub fn mint(conn: &Connection, node: Uuid) -> Result<MintedToken> {
    let mut raw = [0u8; TOKEN_BYTES];
    rand::thread_rng().fill_bytes(&mut raw);
    let plaintext = raw.iter().fold(String::with_capacity(64), |mut s, b| {
        use std::fmt::Write;
        let _ = write!(s, "{b:02x}");
        s
    });

    revoke(conn, node)?;
    conn.execute(
        "INSERT INTO node_tokens (token_hash, node_id, created_at) VALUES (?1,?2,?3)",
        params![
            sha256_hex(plaintext.as_bytes()),
            node.to_string(),
            Timestamp::now().to_rfc3339()
        ],
    )
    .context("storing the node token hash")?;

    Ok(MintedToken {
        plaintext,
        node_id: node,
    })
}

/// Resolve a presented token to the node it authorises.
///
/// Lookup is by hash, so a timing difference here reveals nothing an attacker
/// could use: they would need the preimage.
pub fn resolve(conn: &Connection, presented: &str) -> Result<Option<Uuid>> {
    let hash = sha256_hex(presented.as_bytes());
    let id: Option<String> = conn
        .query_row(
            "SELECT node_id FROM node_tokens WHERE token_hash = ?1",
            params![hash],
            |r| r.get(0),
        )
        .optional()?;
    Ok(match id {
        Some(s) => Some(s.parse()?),
        None => None,
    })
}

/// Invalidate every token for a node. Called on stop and before a re-mint.
pub fn revoke(conn: &Connection, node: Uuid) -> Result<usize> {
    Ok(conn.execute(
        "DELETE FROM node_tokens WHERE node_id = ?1",
        params![node.to_string()],
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem() -> Connection {
        let c = crate::db::open_memory().unwrap();
        c.execute(
            "INSERT INTO nodes (id,name,type,config,x,y,created_at,updated_at)
             VALUES ('11111111-1111-1111-1111-111111111111','a','agent','{}',0,0,'t','t')",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO nodes (id,name,type,config,x,y,created_at,updated_at)
             VALUES ('22222222-2222-2222-2222-222222222222','b','agent','{}',0,0,'t','t')",
            [],
        )
        .unwrap();
        c
    }
    fn n1() -> Uuid {
        "11111111-1111-1111-1111-111111111111".parse().unwrap()
    }
    fn n2() -> Uuid {
        "22222222-2222-2222-2222-222222222222".parse().unwrap()
    }

    #[test]
    fn a_minted_token_resolves_to_its_own_node_and_nothing_else() {
        let c = mem();
        let a = mint(&c, n1()).unwrap();
        let b = mint(&c, n2()).unwrap();

        assert_eq!(resolve(&c, &a.plaintext).unwrap(), Some(n1()));
        assert_eq!(resolve(&c, &b.plaintext).unwrap(), Some(n2()));
        // The whole point: one token, one node.
        assert_ne!(a.plaintext, b.plaintext);
    }

    #[test]
    fn an_unknown_token_resolves_to_nothing_rather_than_erroring() {
        let c = mem();
        mint(&c, n1()).unwrap();
        for bogus in ["", "deadbeef", &"f".repeat(64), "not-hex-at-all"] {
            assert_eq!(
                resolve(&c, bogus).unwrap(),
                None,
                "{bogus:?} must not resolve"
            );
        }
    }

    /// Reading the database must not yield a usable credential.
    #[test]
    fn only_the_hash_is_stored_never_the_token() {
        let c = mem();
        let t = mint(&c, n1()).unwrap();
        let stored: String = c
            .query_row("SELECT token_hash FROM node_tokens", [], |r| r.get(0))
            .unwrap();
        assert_ne!(stored, t.plaintext, "the plaintext must not be stored");
        assert_eq!(stored, sha256_hex(t.plaintext.as_bytes()));
        assert_eq!(stored.len(), 64);
    }

    /// Rotation bounds a leaked token's life to one agent run.
    #[test]
    fn re_minting_invalidates_the_previous_token() {
        let c = mem();
        let old = mint(&c, n1()).unwrap();
        let new = mint(&c, n1()).unwrap();

        assert_eq!(resolve(&c, &new.plaintext).unwrap(), Some(n1()));
        assert_eq!(
            resolve(&c, &old.plaintext).unwrap(),
            None,
            "the old token must be dead after a restart"
        );
        // One live token per node, not an accumulating pile.
        let n: i64 = c
            .query_row("SELECT count(*) FROM node_tokens", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn revoking_kills_the_token_immediately() {
        let c = mem();
        let t = mint(&c, n1()).unwrap();
        assert_eq!(revoke(&c, n1()).unwrap(), 1);
        assert_eq!(resolve(&c, &t.plaintext).unwrap(), None);
    }

    #[test]
    fn tokens_die_with_their_node() {
        let c = mem();
        let t = mint(&c, n1()).unwrap();
        c.execute("DELETE FROM nodes WHERE id = ?1", params![n1().to_string()])
            .unwrap();
        // Cascade, so a deleted node's token cannot outlive it and authorise
        // calls as a node that no longer exists.
        assert_eq!(resolve(&c, &t.plaintext).unwrap(), None);
    }

    #[test]
    fn a_token_is_32_bytes_of_hex() {
        let c = mem();
        let t = mint(&c, n1()).unwrap();
        assert_eq!(t.plaintext.len(), TOKEN_BYTES * 2);
        assert!(t.plaintext.chars().all(|ch| ch.is_ascii_hexdigit()));
    }
}
