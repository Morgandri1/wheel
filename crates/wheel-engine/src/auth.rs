//! Per-node harness credentials.
//!
//! Each agent node gets its own credential directory, which is what lets two
//! agents in one sandbox be two different accounts. API-key mode is stored
//! here; OAuth (paste-code for claude, device-code for codex) writes into the
//! same per-node directory via the harness's own login, so the two modes do not
//! need separate isolation stories.

use std::{
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use anyhow::{bail, Context, Result};
use wheel_core::{CredentialKind, Harness};

/// Filename inside the node's credential dir. Not `.credentials.json`, which is
/// the harness's own file — ours must not collide with it.
///
/// It holds an API key OR a long-lived OAuth token; which one is decided by
/// reading it, never by a second file recording the kind. Two files can drift
/// apart, and the drift would be silent: the wrong env var is exported and the
/// agent fails at request time looking perfectly authenticated.
const TOKEN_FILE: &str = "wheel-token";

/// Prefix of the long-lived OAuth token minted by `claude setup-token` for
/// subscription accounts (as opposed to `sk-ant-api…`, a real API key).
///
/// The two are NOT interchangeable: a setup-token sent as `ANTHROPIC_API_KEY`
/// is rejected by the API, and the operator would see an authentication error
/// with credentials that are perfectly valid — just handed over in the wrong
/// envelope. This prefix is the only thing that distinguishes them.
const OAUTH_TOKEN_PREFIX: &str = "sk-ant-oat";

/// Anthropic credentials in general. Used only to catch a token pasted into
/// the wrong node type.
const ANTHROPIC_PREFIX: &str = "sk-ant-";

/// Which kind of credential this token is, for this harness.
///
/// Only the `sk-ant-oat` prefix is treated as special. Everything else is an
/// API key, deliberately: keys issued by a gateway or proxy do not carry
/// Anthropic's prefixes at all, and refusing them would block a legitimate
/// setup to guard against a mistake that the one recognisable prefix already
/// catches.
pub fn classify_token(token: &str, harness: Harness) -> CredentialKind {
    match harness {
        Harness::Claude if token.trim().starts_with(OAUTH_TOKEN_PREFIX) => {
            CredentialKind::OauthToken
        }
        _ => CredentialKind::ApiKey,
    }
}

/// Which env var carries a credential of this kind to this harness.
pub fn token_env(kind: CredentialKind, harness: Harness) -> &'static str {
    match (harness, kind) {
        (Harness::Claude, CredentialKind::OauthToken) => "CLAUDE_CODE_OAUTH_TOKEN",
        // An OauthSession is never carried by an env var; it lives in the
        // node's config dir. Reaching here with one means a stored token was
        // classified as a session, which cannot happen — but if it ever does,
        // the API key variable is the safe default.
        (Harness::Claude, _) => "ANTHROPIC_API_KEY",
        // Codex has no long-lived-token env var; its OAuth lives in auth.json.
        //
        // `CODEX_API_KEY`, not `OPENAI_API_KEY`: the latter is *noticed* by
        // `codex doctor` and reported as if it were fine, but it is not in
        // codex's auth resolution chain and will not authenticate anything.
        // That trap cost real time to find, so it is encoded here rather than
        // left to memory.
        (Harness::Codex, _) => "CODEX_API_KEY",
    }
}

/// Store a credential for a node, readable only by the uid that will run it,
/// and report which kind it turned out to be.
///
/// Created 0600 at open time rather than chmod-ed afterwards: a key that is
/// briefly world-readable is a key that leaked.
pub fn store_token(config_dir: &Path, key: &str, harness: Harness) -> Result<CredentialKind> {
    let key = key.trim();
    if key.is_empty() {
        bail!("an empty api key is not a credential");
    }
    // An Anthropic credential on a codex node authenticates nothing. Caught
    // here because the alternative is a node that starts, looks fine, and
    // fails on its first turn with an error naming neither the node nor the
    // credential.
    if harness == Harness::Codex && key.starts_with(ANTHROPIC_PREFIX) {
        bail!("that is an Anthropic credential ({ANTHROPIC_PREFIX}…) but this is a codex node");
    }
    std::fs::create_dir_all(config_dir)
        .with_context(|| format!("creating {}", config_dir.display()))?;
    // The directory matters as much as the file: a 0755 parent lets a sibling
    // uid list and read what is inside once per-node uids land.
    set_mode(config_dir, 0o700)?;

    let path = config_dir.join(TOKEN_FILE);
    write_secret(&path, key).with_context(|| format!("writing {}", path.display()))?;
    Ok(classify_token(key, harness))
}

pub fn read_token(config_dir: &Path) -> Option<String> {
    std::fs::read_to_string(config_dir.join(TOKEN_FILE))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// The kind of credential stored for this node, if any.
pub fn stored_token_kind(config_dir: &Path, harness: Harness) -> Option<CredentialKind> {
    read_token(config_dir).map(|t| classify_token(&t, harness))
}

pub fn clear_token(config_dir: &Path) -> Result<()> {
    let path = config_dir.join(TOKEN_FILE);
    if path.exists() {
        std::fs::remove_file(&path)?;
    }
    Ok(())
}

/// Env additions that authenticate a child, if it has stored credentials.
///
/// Exactly one variable is ever set: exporting both would leave which one wins
/// up to the harness's own precedence, which is not something the engine
/// should be guessing at.
pub fn credential_env(config_dir: &Path, harness: Harness) -> Vec<(String, String)> {
    match read_token(config_dir) {
        Some(key) => {
            let kind = classify_token(&key, harness);
            vec![(token_env(kind, harness).to_string(), key)]
        }
        // No key is not an error: the harness may have OAuth credentials in its
        // own config dir, and the authoritative answer comes from probing it.
        None => Vec::new(),
    }
}

/// Does this node have credentials of any kind we can see?
///
/// Deliberately named `has_stored_credentials` rather than `is_authenticated`:
/// a key on disk is not proof it works. Only the harness's own probe
/// (`claude auth status --json`, `codex login status`) can say that, and the
/// engine treats an unprobed node as unknown rather than authenticated.
pub fn has_stored_credentials(config_dir: &Path, harness: Harness) -> bool {
    if read_token(config_dir).is_some() {
        return true;
    }
    // Both locations, because we set both `CLAUDE_CONFIG_DIR`/`CODEX_HOME`
    // AND `HOME` to this directory: the CLI writes to the config dir it was
    // told about, but if it ever falls back to `$HOME`, the file lands one
    // level down instead. The child reads it either way, so treating the
    // second layout as "no credentials" would fail a login that actually
    // worked — the worst answer available here.
    let (dir_name, file) = match harness {
        // Written by `claude auth login`. On Linux there is no keyring, so a
        // plain 0600 file is the whole story.
        Harness::Claude => (".claude", ".credentials.json"),
        Harness::Codex => (".codex", "auth.json"),
    };
    config_dir.join(file).exists() || config_dir.join(dir_name).join(file).exists()
}

/// A credential recovered from the harness's own store, so it can be handed to
/// other agents through a vault.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredOauth {
    pub token: String,
    /// Milliseconds since the epoch, when the store says so. `None` means the
    /// store did not say -- NOT that the token is durable.
    pub expires_at: Option<i64>,
}

impl StoredOauth {
    /// Long-lived tokens from `claude setup-token` carry the `oat` marker and
    /// no expiry. A session access token is the other thing this can find, and
    /// the caller has to be able to tell them apart before copying one into a
    /// vault that five other agents will read.
    pub fn is_long_lived(&self) -> bool {
        self.expires_at.is_none() && self.token.starts_with(OAUTH_TOKEN_PREFIX)
    }
}

/// Pull a value usable as `CLAUDE_CODE_OAUTH_TOKEN` out of the node's own
/// credential store.
///
/// Deliberately shape-tolerant: it looks for an access-token field anywhere in
/// the document and otherwise for anything carrying the `sk-ant-oat` marker,
/// rather than hard-coding a path into a file this engine does not own. If the
/// CLI reorganises its store, this degrades to "could not find it" -- which the
/// caller reports -- instead of silently vaulting the wrong string.
pub fn oauth_token_from_store(
    config_dir: &Path,
    not_before: Option<SystemTime>,
) -> Result<StoredOauth> {
    let path = claude_credentials_path(config_dir, not_before)
        .ok_or_else(|| anyhow::anyhow!("this node has no stored claude credentials"))?;
    let raw =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let doc: serde_json::Value =
        serde_json::from_str(&raw).with_context(|| format!("parsing {}", path.display()))?;

    find_access_token(&doc).ok_or_else(|| {
        anyhow::anyhow!(
            "found {} but no OAuth token in it; the credential may be stored in a form \
             this engine cannot forward (run `claude setup-token` and paste that instead)",
            path.display()
        )
    })
}

/// Where `claude auth login` leaves its credentials, checking both layouts the
/// spawn env can produce.
fn claude_credentials_path(
    config_dir: &Path,
    not_before: Option<SystemTime>,
) -> Option<std::path::PathBuf> {
    // The child's HOME *is* this directory, and an agent is untrusted code
    // (§2). So it can write `.credentials.json` here itself. Two consequences,
    // both handled:
    //
    // 1. Take the NEWEST candidate, not a fixed preference. Preferring the
    //    top-level path meant an agent could plant one there and have it win
    //    over the file the CLI actually wrote a level down.
    // 2. When the caller knows when the login began, refuse anything older.
    //    A credential the agent planted before the operator ever started
    //    signing in is not the credential that login produced -- and vaulting
    //    it would hand an agent-chosen token to every peer on the board.
    let mut best: Option<(std::path::PathBuf, SystemTime)> = None;
    for candidate in [
        config_dir.join(".credentials.json"),
        config_dir.join(".claude").join(".credentials.json"),
    ] {
        let Ok(meta) = std::fs::metadata(&candidate) else {
            continue;
        };
        let Ok(modified) = meta.modified() else {
            continue;
        };
        if let Some(floor) = not_before {
            // Second granularity on some filesystems, so allow the boundary.
            if modified + Duration::from_secs(1) < floor {
                continue;
            }
        }
        if best.as_ref().is_none_or(|(_, t)| modified > *t) {
            best = Some((candidate, modified));
        }
    }
    best.map(|(p, _)| p)
}

fn find_access_token(v: &serde_json::Value) -> Option<StoredOauth> {
    match v {
        serde_json::Value::Object(map) => {
            for (k, val) in map {
                let named_token =
                    k.eq_ignore_ascii_case("accessToken") || k.eq_ignore_ascii_case("access_token");
                if named_token {
                    if let Some(t) = val.as_str().filter(|t| !t.is_empty()) {
                        return Some(StoredOauth {
                            token: t.to_string(),
                            expires_at: expiry_in(map),
                        });
                    }
                }
                // A token by its marker, wherever it sits.
                if let Some(t) = val.as_str().filter(|t| t.starts_with(OAUTH_TOKEN_PREFIX)) {
                    return Some(StoredOauth {
                        token: t.to_string(),
                        expires_at: expiry_in(map),
                    });
                }
            }
            map.values().find_map(find_access_token)
        }
        serde_json::Value::Array(items) => items.iter().find_map(find_access_token),
        _ => None,
    }
}

fn expiry_in(map: &serde_json::Map<String, serde_json::Value>) -> Option<i64> {
    map.iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("expiresAt") || k.eq_ignore_ascii_case("expires_at"))
        .and_then(|(_, v)| v.as_i64())
}

/// `$CODEX_HOME/config.toml` content forcing file-based credential storage.
///
/// `cli_auth_credentials_store` defaults to "auto", which may use the OS
/// keyring — and `CODEX_HOME` does NOT isolate a keyring. Without this, two
/// nodes could collide in one shared store, which would silently break the
/// two-agents-two-accounts property that the per-node dir exists to provide.
pub const CODEX_FILE_STORE_CONFIG: &str = "cli_auth_credentials_store = \"file\"\n";

pub fn ensure_codex_file_store(config_dir: &Path) -> Result<()> {
    std::fs::create_dir_all(config_dir)?;
    set_mode(config_dir, 0o700)?;
    let path = config_dir.join("config.toml");
    if !path.exists() {
        std::fs::write(&path, CODEX_FILE_STORE_CONFIG)?;
    }
    Ok(())
}

fn set_mode(path: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, PermissionsExt::from_mode(mode))?;
    Ok(())
}

fn write_secret(path: &PathBuf, contents: &str) -> Result<()> {
    use std::{io::Write, os::unix::fs::OpenOptionsExt};
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(contents.as_bytes())?;
    f.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("wheel-auth-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        p
    }

    fn mode_of(p: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(p).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn a_stored_key_round_trips() {
        let d = tmp("roundtrip");
        store_token(&d, "sk-ant-secret", Harness::Claude).unwrap();
        assert_eq!(read_token(&d).as_deref(), Some("sk-ant-secret"));
        std::fs::remove_dir_all(&d).ok();
    }

    /// The key and its directory must be unreadable to any other uid from the
    /// moment they exist — not after a later chmod.
    #[test]
    fn the_key_and_its_directory_are_locked_down() {
        let d = tmp("modes");
        store_token(&d, "sk-x", Harness::Claude).unwrap();
        assert_eq!(mode_of(&d), 0o700, "credential dir must be 0700");
        assert_eq!(
            mode_of(&d.join(TOKEN_FILE)),
            0o600,
            "the key file must be 0600"
        );
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn whitespace_is_trimmed_and_an_empty_key_is_refused() {
        let d = tmp("trim");
        // A key pasted from a browser routinely carries a trailing newline;
        // storing it verbatim would send a header the provider rejects.
        store_token(&d, "  sk-padded\n", Harness::Claude).unwrap();
        assert_eq!(read_token(&d).as_deref(), Some("sk-padded"));

        for empty in ["", "   ", "\n"] {
            assert!(
                store_token(&d, empty, Harness::Claude).is_err(),
                "{empty:?} must not be accepted as a credential"
            );
        }
        std::fs::remove_dir_all(&d).ok();
    }

    /// The trap that cost real time: OPENAI_API_KEY is reported as present by
    /// `codex doctor` but is not in codex's auth chain.
    #[test]
    fn codex_uses_codex_api_key_not_openai_api_key() {
        for kind in [CredentialKind::ApiKey, CredentialKind::OauthToken] {
            assert_eq!(token_env(kind, Harness::Codex), "CODEX_API_KEY");
            assert_ne!(token_env(kind, Harness::Codex), "OPENAI_API_KEY");
        }
        assert_eq!(
            token_env(CredentialKind::ApiKey, Harness::Claude),
            "ANTHROPIC_API_KEY"
        );
    }

    /// The operator has a Claude subscription and no API key, so the token
    /// from `claude setup-token` is the ONLY credential they can supply. Sent
    /// as ANTHROPIC_API_KEY it is rejected, and the failure looks like bad
    /// credentials rather than a mis-addressed envelope.
    #[test]
    fn a_setup_token_goes_to_claude_code_oauth_token_not_anthropic_api_key() {
        assert_eq!(
            classify_token("sk-ant-oat01-abc123", Harness::Claude),
            CredentialKind::OauthToken
        );
        assert_eq!(
            token_env(CredentialKind::OauthToken, Harness::Claude),
            "CLAUDE_CODE_OAUTH_TOKEN"
        );

        let d = tmp("oat");
        assert_eq!(
            store_token(&d, "sk-ant-oat01-abc123", Harness::Claude).unwrap(),
            CredentialKind::OauthToken
        );
        // Exactly one variable, and it is the right one: setting both would
        // leave the winner up to the harness's precedence.
        assert_eq!(
            credential_env(&d, Harness::Claude),
            vec![(
                "CLAUDE_CODE_OAUTH_TOKEN".to_string(),
                "sk-ant-oat01-abc123".to_string()
            )]
        );
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn an_api_key_is_still_an_api_key() {
        let d = tmp("apikey");
        assert_eq!(
            store_token(&d, "sk-ant-api03-xyz", Harness::Claude).unwrap(),
            CredentialKind::ApiKey
        );
        assert_eq!(
            credential_env(&d, Harness::Claude),
            vec![(
                "ANTHROPIC_API_KEY".to_string(),
                "sk-ant-api03-xyz".to_string()
            )]
        );
        // A gateway key carries no Anthropic prefix at all and must still work
        // rather than be refused for not looking familiar.
        assert_eq!(
            classify_token("gw_live_0001", Harness::Claude),
            CredentialKind::ApiKey
        );
        std::fs::remove_dir_all(&d).ok();
    }

    /// The kind is derived from the token itself, never from a second file
    /// recording it — two files can drift, and the drift is silent.
    #[test]
    fn the_stored_kind_is_read_back_from_the_token() {
        let d = tmp("kindback");
        store_token(&d, "sk-ant-oat01-q", Harness::Claude).unwrap();
        assert_eq!(
            stored_token_kind(&d, Harness::Claude),
            Some(CredentialKind::OauthToken)
        );
        // Overwriting with the other kind re-routes it, with nothing to sync.
        store_token(&d, "sk-ant-api03-q", Harness::Claude).unwrap();
        assert_eq!(
            stored_token_kind(&d, Harness::Claude),
            Some(CredentialKind::ApiKey)
        );
        clear_token(&d).unwrap();
        assert_eq!(stored_token_kind(&d, Harness::Claude), None);
        std::fs::remove_dir_all(&d).ok();
    }

    /// A Claude credential pasted into a codex node authenticates nothing.
    /// Refused at the door, because the alternative is a node that starts,
    /// looks fine, and fails on its first turn.
    #[test]
    fn an_anthropic_credential_is_refused_on_a_codex_node() {
        let d = tmp("wrongnode");
        for token in ["sk-ant-oat01-a", "sk-ant-api03-a"] {
            let err = store_token(&d, token, Harness::Codex)
                .unwrap_err()
                .to_string();
            assert!(err.contains("codex node"), "unhelpful message: {err}");
        }
        // ...and nothing was written on the way to refusing.
        assert!(read_token(&d).is_none());
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn credential_env_is_empty_without_a_key_rather_than_setting_a_blank_one() {
        let d = tmp("noenv");
        std::fs::create_dir_all(&d).unwrap();
        // Exporting an empty ANTHROPIC_API_KEY would look authenticated and
        // fail at request time, which is the worst of both.
        assert!(credential_env(&d, Harness::Claude).is_empty());

        store_token(&d, "sk-y", Harness::Claude).unwrap();
        assert_eq!(
            credential_env(&d, Harness::Claude),
            vec![("ANTHROPIC_API_KEY".to_string(), "sk-y".to_string())]
        );
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn oauth_credentials_also_count_as_stored() {
        let d = tmp("oauth");
        std::fs::create_dir_all(&d).unwrap();
        assert!(!has_stored_credentials(&d, Harness::Claude));

        std::fs::write(d.join(".credentials.json"), "{}").unwrap();
        assert!(has_stored_credentials(&d, Harness::Claude));
        // ...and the two harnesses look in different places.
        assert!(!has_stored_credentials(&d, Harness::Codex));
        std::fs::write(d.join("auth.json"), "{}").unwrap();
        assert!(has_stored_credentials(&d, Harness::Codex));
        std::fs::remove_dir_all(&d).ok();
    }

    /// We set HOME to the same directory, so a CLI that ignored
    /// CLAUDE_CONFIG_DIR would still write somewhere the child can read.
    /// Calling that "not signed in" would fail a login that worked.
    #[test]
    fn credentials_under_the_home_layout_count_too() {
        let d = tmp("home-layout");
        assert!(!has_stored_credentials(&d, Harness::Claude));

        std::fs::create_dir_all(d.join(".claude")).unwrap();
        std::fs::write(d.join(".claude/.credentials.json"), "{}").unwrap();
        assert!(has_stored_credentials(&d, Harness::Claude));
        assert!(!has_stored_credentials(&d, Harness::Codex));

        std::fs::create_dir_all(d.join(".codex")).unwrap();
        std::fs::write(d.join(".codex/auth.json"), "{}").unwrap();
        assert!(has_stored_credentials(&d, Harness::Codex));
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn clearing_removes_the_key_and_is_idempotent() {
        let d = tmp("clear");
        store_token(&d, "sk-z", Harness::Claude).unwrap();
        clear_token(&d).unwrap();
        assert!(read_token(&d).is_none());
        // Clearing again must not error: stop/restart paths call it blindly.
        clear_token(&d).unwrap();
        std::fs::remove_dir_all(&d).ok();
    }

    /// CODEX_HOME does not isolate the OS keyring, so each node must force
    /// file storage or two nodes can collide in one shared store.
    #[test]
    fn codex_config_forces_file_credential_storage() {
        let d = tmp("codexcfg");
        ensure_codex_file_store(&d).unwrap();
        let cfg = std::fs::read_to_string(d.join("config.toml")).unwrap();
        assert!(cfg.contains("cli_auth_credentials_store = \"file\""));
        assert_eq!(mode_of(&d), 0o700);

        // Must not clobber a config the user or a login already wrote.
        std::fs::write(d.join("config.toml"), "custom = true\n").unwrap();
        ensure_codex_file_store(&d).unwrap();
        assert_eq!(
            std::fs::read_to_string(d.join("config.toml")).unwrap(),
            "custom = true\n"
        );
        std::fs::remove_dir_all(&d).ok();
    }
}

#[cfg(test)]
mod vault_handoff_tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("wheel-oauthstore-{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&d).ok();
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// ADVERSARY finding 018. `save_to_vault` applies to the api_key path too,
    /// and it stored EVERY credential under CLAUDE_CODE_OAUTH_TOKEN. A
    /// provider key vaulted under that name is exported to every peer agent
    /// under a variable the harness does not read -- so a whole board fails to
    /// authenticate with a credential that is present and perfectly valid.
    /// The vault key must be the variable the credential actually is.
    #[test]
    fn a_vaulted_credential_is_keyed_by_the_variable_it_actually_is() {
        let cases = [
            (
                "sk-ant-oat01-abc",
                Harness::Claude,
                "CLAUDE_CODE_OAUTH_TOKEN",
            ),
            ("sk-ant-api03-abc", Harness::Claude, "ANTHROPIC_API_KEY"),
            ("sk-proj-abc", Harness::Codex, "CODEX_API_KEY"),
        ];
        for (token, harness, want) in cases {
            let kind = classify_token(token, harness);
            assert_eq!(
                token_env(kind, harness),
                want,
                "{token} on {harness:?} must be vaulted as {want}"
            );
        }
        // ...and every name it can produce is one the engine recognises as a
        // credential, or the ambiguity rule would never fire on it.
        for (token, harness, _) in cases {
            let k = token_env(classify_token(token, harness), harness);
            assert!(
                wheel_core::is_credential_key(k),
                "{k} must be a recognised credential key"
            );
        }
    }

    /// PM ruling: an explicit `vault_key` is a CONFIRMATION, not an
    /// instruction. This pins the pairs it must accept and reject, so the
    /// route's check cannot drift from the routing it is confirming.
    #[test]
    fn an_explicit_vault_key_is_only_valid_when_it_matches_the_credential() {
        let agrees = |token: &str, harness: Harness, requested: &str| {
            let kind = classify_token(token, harness);
            requested.eq_ignore_ascii_case(token_env(kind, harness))
        };

        // Accepted: the caller named the key the credential actually is.
        assert!(agrees(
            "sk-ant-oat01-x",
            Harness::Claude,
            "CLAUDE_CODE_OAUTH_TOKEN"
        ));
        assert!(agrees(
            "sk-ant-api03-x",
            Harness::Claude,
            "ANTHROPIC_API_KEY"
        ));
        assert!(agrees("sk-x", Harness::Codex, "CODEX_API_KEY"));
        // Case is not the caller's problem; env names are conventionally
        // uppercase and rejecting on case alone would be a puzzle, not a check.
        assert!(agrees(
            "sk-ant-api03-x",
            Harness::Claude,
            "anthropic_api_key"
        ));

        // Refused: this is exactly the 018 mistake, stated by the caller.
        assert!(!agrees(
            "sk-ant-api03-x",
            Harness::Claude,
            "CLAUDE_CODE_OAUTH_TOKEN"
        ));
        assert!(!agrees(
            "sk-ant-oat01-x",
            Harness::Claude,
            "ANTHROPIC_API_KEY"
        ));
        assert!(!agrees("sk-ant-api03-x", Harness::Claude, "CODEX_API_KEY"));
        assert!(!agrees("sk-x", Harness::Codex, "ANTHROPIC_API_KEY"));
        assert!(!agrees("sk-ant-oat01-x", Harness::Claude, "SOMETHING_ELSE"));
    }

    /// The shape `claude auth login` is expected to leave behind.
    #[test]
    fn the_token_is_found_in_the_stores_normal_shape() {
        let d = dir("normal");
        std::fs::write(
            d.join(".credentials.json"),
            r#"{"claudeAiOauth":{"accessToken":"sk-ant-oat01-abc","refreshToken":"sk-ant-ort01-xyz","expiresAt":1799999999000,"subscriptionType":"max"}}"#,
        )
        .unwrap();
        let got = oauth_token_from_store(&d, None).unwrap();
        assert_eq!(got.token, "sk-ant-oat01-abc");
        assert_eq!(got.expires_at, Some(1799999999000));
        // It carries an expiry, so it is NOT the durable credential however
        // much its prefix looks like one.
        assert!(!got.is_long_lived());
        // The refresh token must not be what we picked.
        assert!(!got.token.contains("ort"));
        std::fs::remove_dir_all(&d).ok();
    }

    /// We set HOME to the same directory, so the CLI may write one level down.
    #[test]
    fn the_token_is_found_under_the_home_layout_too() {
        let d = dir("home");
        std::fs::create_dir_all(d.join(".claude")).unwrap();
        std::fs::write(
            d.join(".claude/.credentials.json"),
            r#"{"claudeAiOauth":{"accessToken":"sk-ant-oat01-home"}}"#,
        )
        .unwrap();
        assert_eq!(
            oauth_token_from_store(&d, None).unwrap().token,
            "sk-ant-oat01-home"
        );
        std::fs::remove_dir_all(&d).ok();
    }

    /// A long-lived `claude setup-token` credential has no expiry, and that is
    /// the difference the caller has to be able to see before handing it to
    /// five other agents.
    #[test]
    fn a_durable_token_is_distinguishable_from_a_session_one() {
        let d = dir("durable");
        std::fs::write(
            d.join(".credentials.json"),
            r#"{"accessToken":"sk-ant-oat01-durable"}"#,
        )
        .unwrap();
        assert!(oauth_token_from_store(&d, None).unwrap().is_long_lived());
        std::fs::remove_dir_all(&d).ok();
    }

    /// If the CLI reorganises its store, the honest answer is "I could not
    /// find it" -- vaulting the wrong string would authenticate nothing and
    /// look like success.
    #[test]
    fn an_unrecognisable_store_is_reported_not_guessed() {
        let d = dir("weird");
        std::fs::write(d.join(".credentials.json"), r#"{"something":{"else":1}}"#).unwrap();
        let err = oauth_token_from_store(&d, None).unwrap_err().to_string();
        assert!(err.contains("no OAuth token"), "{err}");
        assert!(
            err.contains("setup-token"),
            "the error must say the way out: {err}"
        );

        // ...and a node that never logged in at all says that instead.
        let empty = dir("empty");
        let err = oauth_token_from_store(&empty, None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("no stored claude credentials"), "{err}");
        std::fs::remove_dir_all(&d).ok();
        std::fs::remove_dir_all(&empty).ok();
    }

    /// The child's HOME *is* the node's credential directory, and an agent is
    /// untrusted code, so it can write `.credentials.json` there itself.
    /// Preferring the top-level path let a planted file beat the one the CLI
    /// actually wrote a level down — and `save_to_vault` would then push an
    /// agent-chosen token to every peer agent on the board.
    #[test]
    fn a_planted_credential_does_not_beat_the_one_the_login_wrote() {
        let d = dir("planted");
        std::fs::create_dir_all(d.join(".claude")).unwrap();

        // The agent gets there first, at the path that used to win.
        std::fs::write(
            d.join(".credentials.json"),
            r#"{"accessToken":"sk-ant-oat01-ATTACKER"}"#,
        )
        .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1100));

        // Then the real login writes its own, one level down.
        let login_started = SystemTime::now();
        std::fs::write(
            d.join(".claude/.credentials.json"),
            r#"{"accessToken":"sk-ant-oat01-REAL"}"#,
        )
        .unwrap();

        // Newest wins, regardless of which path it is on.
        assert_eq!(
            oauth_token_from_store(&d, None).unwrap().token,
            "sk-ant-oat01-REAL"
        );
        // ...and with a freshness floor, the stale planted one is not even a
        // candidate.
        assert_eq!(
            oauth_token_from_store(&d, Some(login_started))
                .unwrap()
                .token,
            "sk-ant-oat01-REAL"
        );
        std::fs::remove_dir_all(&d).ok();
    }

    /// If the ONLY credential present predates the login, there is nothing
    /// this login produced — and saying so beats vaulting whatever was lying
    /// around.
    #[test]
    fn a_credential_older_than_the_login_is_not_evidence_of_a_login() {
        let d = dir("stale-only");
        std::fs::write(
            d.join(".credentials.json"),
            r#"{"accessToken":"sk-ant-oat01-PLANTED"}"#,
        )
        .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1100));
        let login_started = SystemTime::now();

        let err = oauth_token_from_store(&d, Some(login_started))
            .unwrap_err()
            .to_string();
        assert!(err.contains("no stored claude credentials"), "{err}");

        // Without a floor it is still readable — that path only reports an
        // expiry for display and is not a decision about anyone else.
        assert!(oauth_token_from_store(&d, None).is_ok());
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn a_malformed_store_is_an_error_rather_than_a_panic() {
        let d = dir("malformed");
        std::fs::write(d.join(".credentials.json"), "not json at all").unwrap();
        assert!(oauth_token_from_store(&d, None).is_err());
        std::fs::remove_dir_all(&d).ok();
    }
}

#[cfg(test)]
mod setup_token_tests {
    use super::*;

    /// The whole promise of `setup_token` is that it will not expire under
    /// five other agents. That promise is only worth anything if a short-lived
    /// credential submitted to it is REFUSED rather than accepted and vaulted.
    #[test]
    fn only_a_durable_credential_classifies_as_a_setup_token() {
        assert_eq!(
            classify_token("sk-ant-oat01-durable", Harness::Claude),
            CredentialKind::OauthToken
        );
        // A provider API key is a key, not a setup-token, however useful.
        assert_eq!(
            classify_token("sk-ant-api03-key", Harness::Claude),
            CredentialKind::ApiKey
        );
        for not_durable in ["sk-ant-api03-key", "sk-live-something", "", "oat"] {
            assert_ne!(
                classify_token(not_durable, Harness::Claude),
                CredentialKind::OauthToken,
                "{not_durable:?} must not pass as a setup-token"
            );
        }
    }

    /// Whichever field it arrives in, a durable token has to reach the child
    /// as CLAUDE_CODE_OAUTH_TOKEN -- an API key in that variable authenticates
    /// nothing, and the failure looks like a bad credential rather than a
    /// misrouted one.
    #[test]
    fn a_setup_token_reaches_the_child_in_the_right_variable() {
        assert_eq!(
            token_env(CredentialKind::OauthToken, Harness::Claude),
            "CLAUDE_CODE_OAUTH_TOKEN"
        );
        assert_eq!(
            token_env(CredentialKind::ApiKey, Harness::Claude),
            "ANTHROPIC_API_KEY"
        );

        let d = std::env::temp_dir().join(format!("wheel-setuptok-{}", std::process::id()));
        std::fs::remove_dir_all(&d).ok();
        let kind = store_token(&d, "sk-ant-oat01-durable", Harness::Claude).unwrap();
        assert_eq!(kind, CredentialKind::OauthToken);
        let env = credential_env(&d, Harness::Claude);
        assert_eq!(
            env,
            vec![(
                "CLAUDE_CODE_OAUTH_TOKEN".to_string(),
                "sk-ant-oat01-durable".to_string()
            )]
        );
        std::fs::remove_dir_all(&d).ok();
    }

    /// A durable token carries no expiry, which is what suppresses the
    /// "this will expire" warning on the vault response.
    #[test]
    fn a_setup_token_is_reported_as_long_lived() {
        let durable = StoredOauth {
            token: "sk-ant-oat01-durable".into(),
            expires_at: None,
        };
        assert!(durable.is_long_lived());

        let session = StoredOauth {
            token: "sk-ant-oat01-session".into(),
            expires_at: Some(1799999999000),
        };
        assert!(!session.is_long_lived(), "an expiry disqualifies it");
    }
}
