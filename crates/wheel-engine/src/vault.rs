//! Vault nodes: secrets at rest, and the credentials an agent runs with.
//!
//! RECONSTRUCTION NOTE (SDK session B): the original of this file was written
//! by another session and destroyed by a `cat >` from mine at 14:40:10. This
//! is rebuilt from its call sites in api/vault_routes.rs, db/board.rs and
//! supervisor/mod.rs, so the API is exact even where the prose is not.
//!
//! Two rules shape everything here.
//!
//! **A value goes in and never comes back out to the operator.** `PUT` writes,
//! the board returns key NAMES only, and the only readers are the child
//! processes wired to the vault. There is no route that returns a value to a
//! UI, because a UI that can display a secret is a UI that can leak one.
//!
//! **An ambiguous credential is refused, never resolved.** An agent wired to
//! two vaults that both define `ANTHROPIC_API_KEY` has no defensible answer:
//! choosing either silently bills a real person's account. So it is refused at
//! all three doors the ambiguity can walk through — the wire, the write, and
//! the spawn.

use aes_gcm::{
    aead::{Aead, KeyInit, OsRng, Payload},
    AeadCore, Aes256Gcm, Key, Nonce,
};
use anyhow::{bail, Context, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use rusqlite::{params, Connection, OptionalExtension};
use uuid::Uuid;
use wheel_core::{NodeType, WireType};

use crate::db::board;

/// The project's data-encryption key, from `WHEEL_VAULT_KEY`.
pub struct VaultKey {
    cipher: Aes256Gcm,
}

impl VaultKey {
    pub fn from_base64(raw: &str) -> Result<Self> {
        let bytes = B64
            .decode(raw.trim())
            .context("WHEEL_VAULT_KEY must be base64")?;
        if bytes.len() != 32 {
            bail!(
                "WHEEL_VAULT_KEY must decode to 32 bytes for AES-256, got {}",
                bytes.len()
            );
        }
        Ok(Self {
            cipher: Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&bytes)),
        })
    }
}

/// Associated data binding a ciphertext to the exact row it belongs in, so a
/// value lifted from one vault cannot be pasted into another and still decrypt.
fn aad(node: Uuid, key: &str) -> String {
    format!("{node}/{key}")
}

pub fn put(conn: &Connection, vk: &VaultKey, node: Uuid, key: &str, value: &str) -> Result<()> {
    put_with_expiry(conn, vk, node, key, value, None)
}

/// Store a value along with when it stops working, if that is known.
///
/// The expiry travels WITH the value rather than in a side table: a credential
/// copied out of a login is only useful to the UI if "what is stored" and
/// "until when" cannot drift apart.
pub fn put_with_expiry(
    conn: &Connection,
    vk: &VaultKey,
    node: Uuid,
    key: &str,
    value: &str,
    expires_at: Option<wheel_core::Timestamp>,
) -> Result<()> {
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
    let ct = vk
        .cipher
        .encrypt(
            &nonce,
            Payload {
                msg: value.as_bytes(),
                aad: aad(node, key).as_bytes(),
            },
        )
        .map_err(|_| anyhow::anyhow!("could not encrypt the value"))?;
    conn.execute(
        "INSERT INTO vault_values (node_id, key, ciphertext, nonce, updated_at, expires_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(node_id, key) DO UPDATE SET
             ciphertext = excluded.ciphertext,
             nonce = excluded.nonce,
             updated_at = excluded.updated_at,
             expires_at = excluded.expires_at",
        params![
            node.to_string(),
            key,
            ct,
            nonce.to_vec(),
            wheel_core::Timestamp::now().to_string(),
            expires_at.map(|t| t.to_string())
        ],
    )?;
    Ok(())
}

/// When a stored credential stops working, if the store knew.
///
/// `None` covers both "durable" and "nobody told us", which are different
/// facts -- so callers report absence as unknown rather than as safe.
pub fn expiry_of(
    conn: &Connection,
    node: Uuid,
    key: &str,
) -> Result<Option<wheel_core::Timestamp>> {
    let raw: Option<Option<String>> = conn
        .query_row(
            "SELECT expires_at FROM vault_values WHERE node_id = ?1 AND key = ?2",
            params![node.to_string(), key],
            |r| r.get(0),
        )
        .optional()?;
    Ok(raw
        .flatten()
        .and_then(|s| wheel_core::Timestamp::parse_rfc3339(&s).ok()))
}

/// The vault, key and expiry of the credential an agent would actually run
/// with, so the UI can name the account AND say when it lapses.
pub fn credential_detail(
    conn: &Connection,
    agent: Uuid,
    harness: wheel_core::Harness,
) -> Result<Option<(String, String, Option<wheel_core::Timestamp>)>> {
    let recognised: &[&str] = match harness {
        wheel_core::Harness::Claude => &["CLAUDE_CODE_OAUTH_TOKEN", "ANTHROPIC_API_KEY"],
        wheel_core::Harness::Codex => &["CODEX_API_KEY"],
    };
    for (id, name) in wired_vaults(conn, agent)? {
        // STORED values only, not declared keys. A vault that lists
        // ANTHROPIC_API_KEY in its config but holds no value for it supplies
        // nothing: reporting it as a credential tells the operator the agent
        // is authenticated, and then starts a child with no credential at all,
        // which fails on its first request for a reason the UI has just denied.
        //
        // [`find_ambiguity`] agrees: presence is stored-based everywhere (028
        // face 5). A declared-but-empty key can still overlap another vault's
        // declaration — see [`find_declared_overlap`] — but that is a warning,
        // never a block, and never this function's business.
        for key in list_keys(conn, id)? {
            if recognised.contains(&key.as_str()) {
                return Ok(Some((name, key.clone(), expiry_of(conn, id, &key)?)));
            }
        }
    }
    Ok(None)
}

pub fn get(conn: &Connection, vk: &VaultKey, node: Uuid, key: &str) -> Result<Option<String>> {
    let row: Option<(Vec<u8>, Vec<u8>)> = conn
        .query_row(
            "SELECT ciphertext, nonce FROM vault_values WHERE node_id = ?1 AND key = ?2",
            params![node.to_string(), key],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((ct, nonce)) = row else {
        return Ok(None);
    };
    let pt = vk
        .cipher
        .decrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: &ct,
                aad: aad(node, key).as_bytes(),
            },
        )
        .map_err(|_| {
            anyhow::anyhow!("could not decrypt {key}: wrong key, or the value was tampered with")
        })?;
    Ok(Some(
        String::from_utf8(pt).context("a stored secret was not valid utf-8")?,
    ))
}

pub fn delete(conn: &Connection, node: Uuid, key: &str) -> Result<bool> {
    let n = conn.execute(
        "DELETE FROM vault_values WHERE node_id = ?1 AND key = ?2",
        params![node.to_string(), key],
    )?;
    Ok(n > 0)
}

/// Keys a vault DECLARES, from its node config.
///
/// Ambiguity is judged on these rather than on stored values, because a wire
/// is refused before any value exists: a vault that declares
/// `ANTHROPIC_API_KEY` is going to supply it, and telling the operator so at
/// the moment they draw the wire beats telling them at the first `PUT`.
fn declared_keys(conn: &Connection, node: Uuid) -> Result<Vec<String>> {
    Ok(match board::get(conn, node)? {
        Some(n) => match n.config {
            wheel_core::NodeConfig::Vault(v) => v.keys,
            _ => Vec::new(),
        },
        None => Vec::new(),
    })
}

/// Everything a vault might supply: what it declares, plus anything actually
/// stored. The two are kept in step on write, so this is belt and braces —
/// but a key present in only one of them is still a key the agent would get.
fn offered_keys(conn: &Connection, node: Uuid) -> Result<Vec<String>> {
    let mut keys = declared_keys(conn, node)?;
    for k in list_keys(conn, node)? {
        if !keys.contains(&k) {
            keys.push(k);
        }
    }
    keys.sort();
    Ok(keys)
}

/// Key NAMES only. This is what the board and `wheel secret list` may show.
pub fn list_keys(conn: &Connection, node: Uuid) -> Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT key FROM vault_values WHERE node_id = ?1 ORDER BY key")?;
    let keys = stmt
        .query_map(params![node.to_string()], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(keys)
}

/// Every agent with a `read` wire to this vault.
pub fn agents_reading(conn: &Connection, vault: Uuid) -> Result<Vec<Uuid>> {
    let mut stmt = conn.prepare("SELECT from_id FROM wires WHERE to_id = ?1 AND type = 'read'")?;
    let ids: Vec<String> = stmt
        .query_map(params![vault.to_string()], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    drop(stmt);
    let mut out = Vec::new();
    for id in ids {
        if let Ok(uuid) = Uuid::parse_str(&id) {
            if let Some(n) = board::get(conn, uuid)? {
                if n.node_type() == NodeType::Agent {
                    out.push(uuid);
                }
            }
        }
    }
    Ok(out)
}

/// Vaults an agent may read, by (id, name), ordered by name so the ambiguity
/// an operator is shown is the same one every time rather than depending on
/// wire insertion order.
fn wired_vaults(conn: &Connection, agent: Uuid) -> Result<Vec<(Uuid, String)>> {
    let Some(node) = board::get(conn, agent)? else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for w in &node.wires {
        if w.wire_type != WireType::Read {
            continue;
        }
        if let Some(t) = board::get(conn, w.to)? {
            if t.node_type() == NodeType::Vault {
                out.push((t.id, t.name.to_string()));
            }
        }
    }
    out.sort_by(|a, b| a.1.cmp(&b.1));
    Ok(out)
}

/// The name of a vault, other than `exclude`, that already supplies `key` to
/// this agent.
pub fn supplies_key(
    conn: &Connection,
    agent: Uuid,
    key: &str,
    exclude: Uuid,
) -> Result<Option<String>> {
    for (id, name) in wired_vaults(conn, agent)? {
        if id == exclude {
            continue;
        }
        if offered_keys(conn, id)?.iter().any(|k| k == key) {
            return Ok(Some(name));
        }
    }
    Ok(None)
}

/// A key offered to one agent by two vaults.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "ambiguous {what} {key}: both {first} and {second} supply it; \
         remove it from one, or wire the agent to only one of them"
)]
pub struct Ambiguity {
    pub what: &'static str,
    pub key: String,
    pub first: String,
    pub second: String,
}

/// Find a duplicated key among the vaults an agent can read, judging on
/// `keys_of` — the caller decides whether "offers" means declared or stored.
///
/// `adding` lets a wire be judged BEFORE it exists, which is what makes the
/// check at wire-creation time possible at all.
fn find_ambiguity_by(
    conn: &Connection,
    agent: Uuid,
    adding: Option<Uuid>,
    keys_of: impl Fn(&Connection, Uuid) -> Result<Vec<String>>,
) -> Result<Option<Ambiguity>> {
    let mut vaults = wired_vaults(conn, agent)?;
    if let Some(new) = adding {
        if !vaults.iter().any(|(id, _)| *id == new) {
            if let Some(n) = board::get(conn, new)? {
                if n.node_type() == NodeType::Vault {
                    vaults.push((n.id, n.name.to_string()));
                    vaults.sort_by(|a, b| a.1.cmp(&b.1));
                }
            }
        }
    }

    let mut seen: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for (id, name) in vaults {
        for key in keys_of(conn, id)? {
            if let Some(first) = seen.get(&key) {
                return Ok(Some(Ambiguity {
                    what: if wheel_core::is_credential_key(&key) {
                        "credential"
                    } else {
                        "vault key"
                    },
                    key,
                    first: first.clone(),
                    second: name,
                }));
            }
            seen.insert(key, name.clone());
        }
    }
    Ok(None)
}

/// Find a key two of an agent's vaults both actually HOLD a value for.
///
/// Judged on stored values only, not on declaration: an agent must never run
/// with a real choice between two accounts, but a vault that merely declares
/// a key it has not yet been given is not a competing account. This is the
/// only ambiguity that blocks — a wire creation or an agent start.
pub fn find_ambiguity(
    conn: &Connection,
    agent: Uuid,
    adding: Option<Uuid>,
) -> Result<Option<Ambiguity>> {
    find_ambiguity_by(conn, agent, adding, list_keys)
}

/// Find a key two of an agent's vaults both DECLARE, whether or not either
/// has a value yet.
///
/// Non-blocking by design (PM ruling on 028 face 5): declaring a key is a
/// statement of intent, not a competing credential, so this is surfaced as a
/// create-time warning rather than refusing the wire. Skips any pair
/// [`find_ambiguity`] already refuses, since that case never reaches here.
pub fn find_declared_overlap(
    conn: &Connection,
    agent: Uuid,
    adding: Option<Uuid>,
) -> Result<Option<Ambiguity>> {
    find_ambiguity_by(conn, agent, adding, declared_keys)
}

/// The environment an agent gets from its wired vaults.
///
/// Every key is exported, not only credentials: a vault is how an agent gets
/// any secret it needs. An ambiguity here is an error rather than a choice —
/// refusing to start is unhelpful, but an agent quietly running as the wrong
/// account is worse.
pub fn env_for_agent(
    conn: &Connection,
    vk: &VaultKey,
    agent: Uuid,
) -> Result<Vec<(String, String)>> {
    if let Some(a) = find_ambiguity(conn, agent, None)? {
        bail!(a);
    }
    let mut env = Vec::new();
    for (id, _) in wired_vaults(conn, agent)? {
        for key in list_keys(conn, id)? {
            if let Some(v) = get(conn, vk, id, &key)? {
                env.push((key, v));
            }
        }
    }
    Ok(env)
}

/// Blank out any secret that appears in a line bound for a log or transcript.
///
/// Accidental-echo protection ONLY. An agent is untrusted code holding these
/// values in its own environment; if it chooses to transform one before
/// printing it, nothing here catches that. This is not a containment boundary
/// and must not be recorded as one.
pub fn redact(line: &str, secrets: &[String]) -> String {
    let mut out = line.to_string();
    for s in secrets {
        // Very short values would match everywhere and turn a log into noise;
        // a real credential is never this small.
        if s.len() < 8 {
            continue;
        }
        if out.contains(s.as_str()) {
            out = out.replace(s.as_str(), "«redacted»");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use wheel_core::{AgentConfig, NodeConfig, NodeName, Position, VaultConfig};

    fn key() -> VaultKey {
        VaultKey::from_base64(&base64::engine::general_purpose::STANDARD.encode([7u8; 32])).unwrap()
    }

    fn node(name: &str, config: NodeConfig) -> wheel_core::Node {
        wheel_core::Node::new(
            Uuid::new_v4(),
            NodeName::new(name).unwrap(),
            Position::default(),
            config,
        )
    }

    fn vault(name: &str, keys: &[&str]) -> wheel_core::Node {
        node(
            name,
            NodeConfig::Vault(VaultConfig {
                keys: keys.iter().map(|k| k.to_string()).collect(),
            }),
        )
    }

    /// The board is what the UI renders and what every agent can read. A
    /// vault's KEY NAMES belong there; its values never do -- and this is the
    /// path a paste-code login now writes through, so it is worth asserting
    /// rather than assuming.
    #[test]
    fn a_vaulted_credential_is_never_on_the_board() {
        const SECRET: &str = "sk-ant-oat01-this-must-never-appear";
        let c = crate::db::open_memory().unwrap();
        let v = vault("creds", &[]);
        board::create(&c, &v).unwrap();
        put(&c, &key(), v.id, "CLAUDE_CODE_OAUTH_TOKEN", SECRET).unwrap();

        // Whatever the board hands out, in any shape, must not contain it.
        let one = serde_json::to_string(&board::get(&c, v.id).unwrap().unwrap()).unwrap();
        let all = serde_json::to_string(&board::list(&c).unwrap()).unwrap();
        for rendered in [&one, &all] {
            assert!(
                !rendered.contains(SECRET),
                "a vault value reached the board: {rendered}"
            );
        }

        // The KEY NAME is retrievable, because the operator has to see what a
        // vault holds to reason about it. `list_keys` reads what is STORED;
        // the node's declared list is the route's bookkeeping, and the
        // ambiguity checks use the union of the two so a key present in only
        // one of them is still treated as supplied.
        assert_eq!(list_keys(&c, v.id).unwrap(), ["CLAUDE_CODE_OAUTH_TOKEN"]);
        assert_eq!(offered_keys(&c, v.id).unwrap(), ["CLAUDE_CODE_OAUTH_TOKEN"]);

        // And the value is retrievable only through the vault itself.
        assert_eq!(
            get(&c, &key(), v.id, "CLAUDE_CODE_OAUTH_TOKEN")
                .unwrap()
                .unwrap(),
            SECRET
        );
    }

    /// AUTH-declared-key-not-credential. A vault node's config can LIST a key
    /// with no value stored for it — that is the normal state between
    /// creating the vault and the operator pasting the secret in.
    ///
    /// Counting a declared key as a credential told the operator the agent was
    /// authenticated and then started a child with no credential at all, which
    /// fails on its first request for the exact reason the UI had just denied.
    #[test]
    fn a_declared_key_with_no_value_is_not_a_credential() {
        let c = crate::db::open_memory().unwrap();
        // Declared in the config, nothing stored.
        let v = vault("creds", &["ANTHROPIC_API_KEY"]);
        let a = node("worker", NodeConfig::Agent(AgentConfig::default()));
        board::create(&c, &v).unwrap();
        board::create(&c, &a).unwrap();
        board::add_wire(&c, a.id, v.id, wheel_core::WireType::Read, None).unwrap();

        assert_eq!(
            credential_detail(&c, a.id, wheel_core::Harness::Claude).unwrap(),
            None,
            "a declared key supplies nothing until a value is stored"
        );
        // ...and nothing is exported to the child either, so the two agree.
        assert!(env_for_agent(&c, &key(), a.id).unwrap().is_empty());

        // Store the value: NOW it is a credential.
        put(&c, &key(), v.id, "ANTHROPIC_API_KEY", "sk-ant-api03-real").unwrap();
        let (name, k, _) = credential_detail(&c, a.id, wheel_core::Harness::Claude)
            .unwrap()
            .unwrap();
        assert_eq!(name, "creds");
        assert_eq!(k, "ANTHROPIC_API_KEY");

        // Remove it and it stops being one, rather than lingering because the
        // config still lists the name.
        delete(&c, v.id, "ANTHROPIC_API_KEY").unwrap();
        assert_eq!(
            credential_detail(&c, a.id, wheel_core::Harness::Claude).unwrap(),
            None,
            "a removed value must not still read as authenticated"
        );
    }

    /// 028 face 5 (PM overrule): a declared-but-unfilled key must never block
    /// wiring the vault that actually holds the value. `find_ambiguity` is
    /// stored-based, full stop; the declared/declared clash still exists but
    /// only as [`find_declared_overlap`], which is not consulted for a block.
    #[test]
    fn a_declared_but_empty_key_does_not_block_the_vault_with_the_real_value() {
        let c = crate::db::open_memory().unwrap();
        let a = node("worker", NodeConfig::Agent(AgentConfig::default()));
        let v1 = vault("alice", &["ANTHROPIC_API_KEY"]); // declares, never filled
        let v2 = vault("bob", &["ANTHROPIC_API_KEY"]);
        for n in [&a, &v1, &v2] {
            board::create(&c, n).unwrap();
        }
        board::add_wire(&c, a.id, v1.id, wheel_core::WireType::Read, None).unwrap();
        put(&c, &key(), v2.id, "ANTHROPIC_API_KEY", "sk-ant-api03-real").unwrap();

        assert!(
            find_ambiguity(&c, a.id, Some(v2.id)).unwrap().is_none(),
            "a declared-but-empty key in another vault must not block the real one"
        );

        // Two vaults that both merely DECLARE the same key still overlap —
        // just as a non-blocking signal, not a refusal.
        let overlap = find_declared_overlap(&c, a.id, Some(v2.id)).unwrap();
        assert_eq!(
            overlap.map(|o| o.key),
            Some("ANTHROPIC_API_KEY".to_string())
        );
    }

    /// Two vaults that both actually HOLD a value for the same key are a real
    /// conflict: the agent would run as whichever one happened to win. This is
    /// the one case [`find_ambiguity`] still blocks.
    #[test]
    fn two_stored_values_for_the_same_key_are_still_blocked() {
        let c = crate::db::open_memory().unwrap();
        let a = node("worker", NodeConfig::Agent(AgentConfig::default()));
        let v1 = vault("alice", &[]);
        let v2 = vault("bob", &[]);
        for n in [&a, &v1, &v2] {
            board::create(&c, n).unwrap();
        }
        board::add_wire(&c, a.id, v1.id, wheel_core::WireType::Read, None).unwrap();
        put(&c, &key(), v1.id, "ANTHROPIC_API_KEY", "sk-ant-api03-one").unwrap();
        put(&c, &key(), v2.id, "ANTHROPIC_API_KEY", "sk-ant-api03-two").unwrap();

        let clash = find_ambiguity(&c, a.id, Some(v2.id)).unwrap();
        assert_eq!(
            clash.map(|c| c.key),
            Some("ANTHROPIC_API_KEY".to_string()),
            "two REAL values for one key must still clash"
        );
    }

    /// The UI can only warn "re-login by ..." if the expiry survives beside
    /// the value. It is stored on the row so the two cannot drift.
    #[test]
    fn an_expiry_round_trips_with_the_value_it_belongs_to() {
        let c = crate::db::open_memory().unwrap();
        let v = vault("creds", &[]);
        board::create(&c, &v).unwrap();

        let when = wheel_core::Timestamp::parse_rfc3339("2026-09-06T12:00:00Z").unwrap();
        put_with_expiry(
            &c,
            &key(),
            v.id,
            "CLAUDE_CODE_OAUTH_TOKEN",
            "tok",
            Some(when),
        )
        .unwrap();
        assert_eq!(
            expiry_of(&c, v.id, "CLAUDE_CODE_OAUTH_TOKEN").unwrap(),
            Some(when)
        );

        // Absent means durable OR unknown -- never "already expired".
        put(&c, &key(), v.id, "ANTHROPIC_API_KEY", "k").unwrap();
        assert_eq!(expiry_of(&c, v.id, "ANTHROPIC_API_KEY").unwrap(), None);

        // Overwriting without an expiry CLEARS it: the new value is a
        // different credential and must not inherit the old one's deadline.
        put(&c, &key(), v.id, "CLAUDE_CODE_OAUTH_TOKEN", "tok2").unwrap();
        assert_eq!(
            expiry_of(&c, v.id, "CLAUDE_CODE_OAUTH_TOKEN").unwrap(),
            None
        );
    }

    /// `GET auth` needs the vault name AND the deadline in one answer.
    #[test]
    fn credential_detail_names_the_vault_the_key_and_the_deadline() {
        let c = crate::db::open_memory().unwrap();
        let v = vault("anthropic-alice", &[]);
        let a = node("worker", NodeConfig::Agent(AgentConfig::default()));
        board::create(&c, &v).unwrap();
        board::create(&c, &a).unwrap();
        board::add_wire(&c, a.id, v.id, wheel_core::WireType::Read, None).unwrap();

        let when = wheel_core::Timestamp::parse_rfc3339("2026-09-06T12:00:00Z").unwrap();
        put_with_expiry(
            &c,
            &key(),
            v.id,
            "CLAUDE_CODE_OAUTH_TOKEN",
            "tok",
            Some(when),
        )
        .unwrap();

        let (name, k, exp) = credential_detail(&c, a.id, wheel_core::Harness::Claude)
            .unwrap()
            .unwrap();
        assert_eq!(name, "anthropic-alice");
        assert_eq!(k, "CLAUDE_CODE_OAUTH_TOKEN");
        assert_eq!(exp, Some(when));

        // A codex agent is not authenticated by an Anthropic credential.
        assert!(credential_detail(&c, a.id, wheel_core::Harness::Codex)
            .unwrap()
            .is_none());
    }

    /// ADVERSARY 021. The spawn gate reads the VAULT's expiry, per agent — so
    /// one expiring credential in a vault read by N agents stops all N at
    /// once, not just the one that authenticated. This pins the blast radius
    /// the refusal exists to prevent.
    #[test]
    fn one_expiry_on_a_shared_vault_reaches_every_reader() {
        let c = crate::db::open_memory().unwrap();
        let v = vault("team", &[]);
        board::create(&c, &v).unwrap();

        let agents: Vec<_> = ["alice", "bob", "carol"]
            .iter()
            .map(|n| {
                let a = node(n, NodeConfig::Agent(AgentConfig::default()));
                board::create(&c, &a).unwrap();
                board::add_wire(&c, a.id, v.id, wheel_core::WireType::Read, None).unwrap();
                a
            })
            .collect();

        let past = wheel_core::Timestamp::parse_rfc3339("2020-01-01T00:00:00Z").unwrap();
        put_with_expiry(
            &c,
            &key(),
            v.id,
            "CLAUDE_CODE_OAUTH_TOKEN",
            "tok",
            Some(past),
        )
        .unwrap();

        // Every reader sees the same lapsed credential, not just the one that
        // put it there.
        for a in &agents {
            let (name, _k, exp) = credential_detail(&c, a.id, wheel_core::Harness::Claude)
                .unwrap()
                .unwrap();
            assert_eq!(name, "team");
            assert_eq!(exp, Some(past), "{} must see the vault's expiry", a.name);
        }

        // ...and the readers are exactly who a refusal must name.
        let mut readers: Vec<String> = agents_reading(&c, v.id)
            .unwrap()
            .into_iter()
            .filter_map(|id| {
                board::get(&c, id)
                    .ok()
                    .flatten()
                    .map(|n| n.name.to_string())
            })
            .collect();
        readers.sort();
        assert_eq!(readers, ["alice", "bob", "carol"]);
    }

    /// If a child echoes a vaulted credential, it must not survive into a log
    /// line or a transcript that the operator or another agent can read.
    #[test]
    fn a_vaulted_credential_is_redacted_out_of_anything_a_child_echoes() {
        const SECRET: &str = "sk-ant-oat01-this-must-never-appear";
        let line = format!("running with token {SECRET} now");
        let cleaned = redact(&line, &[SECRET.to_string()]);
        assert!(!cleaned.contains(SECRET), "{cleaned}");
        assert!(cleaned.contains("running with token"), "{cleaned}");
    }

    #[test]
    fn a_value_round_trips_and_is_not_stored_in_the_clear() {
        let c = crate::db::open_memory().unwrap();
        let v = vault("creds", &[]);
        board::create(&c, &v).unwrap();
        let vk = key();

        put(&c, &vk, v.id, "ANTHROPIC_API_KEY", "sk-ant-api03-secret").unwrap();
        assert_eq!(
            get(&c, &vk, v.id, "ANTHROPIC_API_KEY").unwrap().as_deref(),
            Some("sk-ant-api03-secret")
        );

        let stored: Vec<u8> = c
            .query_row(
                "SELECT ciphertext FROM vault_values WHERE node_id = ?1",
                params![v.id.to_string()],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            !String::from_utf8_lossy(&stored).contains("sk-ant"),
            "the plaintext must not be recoverable from the row"
        );
    }

    /// A row moved to another key must fail to decrypt rather than quietly
    /// becoming that other secret -- which would silently swap one account's
    /// credential for another's.
    #[test]
    fn ciphertext_is_bound_to_its_vault_and_key() {
        let c = crate::db::open_memory().unwrap();
        let v = vault("creds", &[]);
        board::create(&c, &v).unwrap();
        let vk = key();
        put(&c, &vk, v.id, "A", "value-of-a").unwrap();

        c.execute(
            "UPDATE vault_values SET key = 'B' WHERE node_id = ?1 AND key = 'A'",
            params![v.id.to_string()],
        )
        .unwrap();
        assert!(
            get(&c, &vk, v.id, "B").is_err(),
            "a relabelled row must not decrypt as the new key"
        );
    }

    #[test]
    fn a_different_project_key_cannot_read_the_values() {
        let c = crate::db::open_memory().unwrap();
        let v = vault("creds", &[]);
        board::create(&c, &v).unwrap();
        put(&c, &key(), v.id, "K", "secret").unwrap();

        let other =
            VaultKey::from_base64(&base64::engine::general_purpose::STANDARD.encode([9u8; 32]))
                .unwrap();
        assert!(get(&c, &other, v.id, "K").is_err());
    }

    #[test]
    fn a_vault_key_must_be_thirty_two_bytes() {
        use base64::engine::general_purpose::STANDARD as B;
        assert!(VaultKey::from_base64(&B.encode([1u8; 16])).is_err());
        assert!(VaultKey::from_base64(&B.encode([1u8; 32])).is_ok());
        assert!(VaultKey::from_base64("not base64!!").is_err());
    }

    /// The rule PM set: two vaults offering the same credential is refused,
    /// never resolved, because resolving it means choosing an account for the
    /// user and being silent about it. "Offering" means a STORED value (028
    /// face 5) — a declaration alone does not compete.
    #[test]
    fn two_vaults_offering_the_same_credential_are_ambiguous() {
        let c = crate::db::open_memory().unwrap();
        let a = node("worker", NodeConfig::Agent(AgentConfig::default()));
        let personal = vault("personal", &["ANTHROPIC_API_KEY"]);
        let work = vault("work", &["ANTHROPIC_API_KEY"]);
        for n in [&a, &personal, &work] {
            board::create(&c, n).unwrap();
        }
        put(
            &c,
            &key(),
            personal.id,
            "ANTHROPIC_API_KEY",
            "sk-ant-api03-personal",
        )
        .unwrap();
        put(
            &c,
            &key(),
            work.id,
            "ANTHROPIC_API_KEY",
            "sk-ant-api03-work",
        )
        .unwrap();

        // One vault: fine.
        board::add_wire(&c, a.id, personal.id, WireType::Read, None).unwrap();
        assert!(find_ambiguity(&c, a.id, None).unwrap().is_none());

        // The second is refused at creation, naming both vaults and the key.
        let err = board::add_wire(&c, a.id, work.id, WireType::Read, None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("ambiguous credential"), "{err}");
        assert!(err.contains("ANTHROPIC_API_KEY"), "{err}");
        assert!(err.contains("personal") && err.contains("work"), "{err}");
    }

    /// Different accounts of the same provider is the POINT, so two vaults
    /// holding different keys must be allowed.
    #[test]
    fn two_vaults_with_different_keys_are_fine() {
        let c = crate::db::open_memory().unwrap();
        let a = node("worker", NodeConfig::Agent(AgentConfig::default()));
        let anthropic = vault("anthropic", &["ANTHROPIC_API_KEY"]);
        let openai = vault("openai", &["CODEX_API_KEY"]);
        for n in [&a, &anthropic, &openai] {
            board::create(&c, n).unwrap();
        }
        board::add_wire(&c, a.id, anthropic.id, WireType::Read, None).unwrap();
        board::add_wire(&c, a.id, openai.id, WireType::Read, None).unwrap();
        assert!(find_ambiguity(&c, a.id, None).unwrap().is_none());
    }

    /// Two agents may each have their OWN account. The ambiguity is per agent,
    /// not per board -- otherwise the multi-account feature would forbid
    /// itself.
    #[test]
    fn two_agents_may_use_different_vaults_for_the_same_key() {
        let c = crate::db::open_memory().unwrap();
        let one = node("one", NodeConfig::Agent(AgentConfig::default()));
        let two = node("two", NodeConfig::Agent(AgentConfig::default()));
        let personal = vault("personal", &["ANTHROPIC_API_KEY"]);
        let work = vault("work", &["ANTHROPIC_API_KEY"]);
        for n in [&one, &two, &personal, &work] {
            board::create(&c, n).unwrap();
        }
        board::add_wire(&c, one.id, personal.id, WireType::Read, None).unwrap();
        board::add_wire(&c, two.id, work.id, WireType::Read, None).unwrap();
        assert!(find_ambiguity(&c, one.id, None).unwrap().is_none());
        assert!(find_ambiguity(&c, two.id, None).unwrap().is_none());
    }

    /// The spawn-time check is the last line: a board can reach an ambiguous
    /// state without passing the other two (an import, or wires written before
    /// the rule existed), and starting anyway would pick an account silently.
    #[test]
    fn env_for_agent_refuses_rather_than_choosing_a_winner() {
        let c = crate::db::open_memory().unwrap();
        let a = node("worker", NodeConfig::Agent(AgentConfig::default()));
        let one = vault("one", &["ANTHROPIC_API_KEY"]);
        let two = vault("two", &["ANTHROPIC_API_KEY"]);
        for n in [&a, &one, &two] {
            board::create(&c, n).unwrap();
        }
        put(&c, &key(), one.id, "ANTHROPIC_API_KEY", "sk-ant-api03-one").unwrap();
        put(&c, &key(), two.id, "ANTHROPIC_API_KEY", "sk-ant-api03-two").unwrap();
        // Straight into the table, bypassing add_wire's check exactly as a
        // restored export would.
        for v in [&one, &two] {
            c.execute(
                "INSERT INTO wires (from_id,to_id,type,granted_by,created_at)
                 VALUES (?1,?2,'read',NULL,?3)",
                params![
                    a.id.to_string(),
                    v.id.to_string(),
                    wheel_core::Timestamp::now().to_string()
                ],
            )
            .unwrap();
        }
        let err = env_for_agent(&c, &key(), a.id).unwrap_err().to_string();
        assert!(err.contains("ambiguous credential"), "{err}");
    }

    #[test]
    fn only_wired_vaults_reach_an_agent() {
        let c = crate::db::open_memory().unwrap();
        let a = node("worker", NodeConfig::Agent(AgentConfig::default()));
        let mine = vault("mine", &[]);
        let theirs = vault("theirs", &[]);
        for n in [&a, &mine, &theirs] {
            board::create(&c, n).unwrap();
        }
        board::add_wire(&c, a.id, mine.id, WireType::Read, None).unwrap();
        let vk = key();
        put(&c, &vk, mine.id, "MINE", "m").unwrap();
        put(&c, &vk, theirs.id, "THEIRS", "t").unwrap();

        let env = env_for_agent(&c, &vk, a.id).unwrap();
        assert_eq!(env, vec![("MINE".to_string(), "m".to_string())]);
    }

    #[test]
    fn redaction_replaces_secrets_but_leaves_short_strings_alone() {
        let secrets = vec!["sk-ant-api03-longenough".to_string(), "ab".to_string()];
        let out = redact("token is sk-ant-api03-longenough ok", &secrets);
        assert!(!out.contains("sk-ant-api03-longenough"));
        assert!(out.contains("«redacted»"));
        // A two-character "secret" would turn ordinary prose into confetti.
        assert_eq!(redact("a cab in ab", &secrets), "a cab in ab");
    }
}
