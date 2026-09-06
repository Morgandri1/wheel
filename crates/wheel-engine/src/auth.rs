//! Per-node harness credentials.
//!
//! Each agent node gets its own credential directory, which is what lets two
//! agents in one sandbox be two different accounts. API-key mode is stored
//! here; OAuth (paste-code for claude, device-code for codex) writes into the
//! same per-node directory via the harness's own login, so the two modes do not
//! need separate isolation stories.

use std::path::{Path, PathBuf};

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
