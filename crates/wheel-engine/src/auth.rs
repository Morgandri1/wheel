//! Per-node harness credentials.
//!
//! Each agent node gets its own credential directory, which is what lets two
//! agents in one sandbox be two different accounts. API-key mode is stored
//! here; OAuth (paste-code for claude, device-code for codex) writes into the
//! same per-node directory via the harness's own login, so the two modes do not
//! need separate isolation stories.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use wheel_core::Harness;

/// Filename inside the node's credential dir. Not `.credentials.json`, which is
/// the harness's own file — ours must not collide with it.
const API_KEY_FILE: &str = "wheel-api-key";

/// How an agent node is authenticated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthMode {
    ApiKey,
    /// The harness's own OAuth credentials, written by its login flow.
    Oauth,
}

/// Store an API key for a node, readable only by the uid that will run it.
///
/// Created 0600 at open time rather than chmod-ed afterwards: a key that is
/// briefly world-readable is a key that leaked.
pub fn store_api_key(config_dir: &Path, key: &str) -> Result<()> {
    let key = key.trim();
    if key.is_empty() {
        bail!("an empty api key is not a credential");
    }
    std::fs::create_dir_all(config_dir)
        .with_context(|| format!("creating {}", config_dir.display()))?;
    // The directory matters as much as the file: a 0755 parent lets a sibling
    // uid list and read what is inside once per-node uids land.
    set_mode(config_dir, 0o700)?;

    let path = config_dir.join(API_KEY_FILE);
    write_secret(&path, key).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

pub fn read_api_key(config_dir: &Path) -> Option<String> {
    std::fs::read_to_string(config_dir.join(API_KEY_FILE))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

pub fn clear_api_key(config_dir: &Path) -> Result<()> {
    let path = config_dir.join(API_KEY_FILE);
    if path.exists() {
        std::fs::remove_file(&path)?;
    }
    Ok(())
}

/// Which env var carries an API key for this harness.
///
/// `CODEX_API_KEY`, not `OPENAI_API_KEY`: the latter is *noticed* by
/// `codex doctor` and reported as if it were fine, but it is not in codex's
/// auth resolution chain and will not authenticate anything. That trap cost
/// real time to find, so it is encoded here rather than left to memory.
pub fn api_key_env(harness: Harness) -> &'static str {
    match harness {
        Harness::Claude => "ANTHROPIC_API_KEY",
        Harness::Codex => "CODEX_API_KEY",
    }
}

/// Env additions that authenticate a child, if it has stored credentials.
pub fn credential_env(config_dir: &Path, harness: Harness) -> Vec<(String, String)> {
    match read_api_key(config_dir) {
        Some(key) => vec![(api_key_env(harness).to_string(), key)],
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
    if read_api_key(config_dir).is_some() {
        return true;
    }
    match harness {
        // Written by `claude auth login`. On Linux there is no keyring, so a
        // plain 0600 file is the whole story.
        Harness::Claude => config_dir.join(".credentials.json").exists(),
        Harness::Codex => config_dir.join("auth.json").exists(),
    }
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
        store_api_key(&d, "sk-ant-secret").unwrap();
        assert_eq!(read_api_key(&d).as_deref(), Some("sk-ant-secret"));
        std::fs::remove_dir_all(&d).ok();
    }

    /// The key and its directory must be unreadable to any other uid from the
    /// moment they exist — not after a later chmod.
    #[test]
    fn the_key_and_its_directory_are_locked_down() {
        let d = tmp("modes");
        store_api_key(&d, "sk-x").unwrap();
        assert_eq!(mode_of(&d), 0o700, "credential dir must be 0700");
        assert_eq!(
            mode_of(&d.join(API_KEY_FILE)),
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
        store_api_key(&d, "  sk-padded\n").unwrap();
        assert_eq!(read_api_key(&d).as_deref(), Some("sk-padded"));

        for empty in ["", "   ", "\n"] {
            assert!(
                store_api_key(&d, empty).is_err(),
                "{empty:?} must not be accepted as a credential"
            );
        }
        std::fs::remove_dir_all(&d).ok();
    }

    /// The trap that cost real time: OPENAI_API_KEY is reported as present by
    /// `codex doctor` but is not in codex's auth chain.
    #[test]
    fn codex_uses_codex_api_key_not_openai_api_key() {
        assert_eq!(api_key_env(Harness::Codex), "CODEX_API_KEY");
        assert_ne!(api_key_env(Harness::Codex), "OPENAI_API_KEY");
        assert_eq!(api_key_env(Harness::Claude), "ANTHROPIC_API_KEY");
    }

    #[test]
    fn credential_env_is_empty_without_a_key_rather_than_setting_a_blank_one() {
        let d = tmp("noenv");
        std::fs::create_dir_all(&d).unwrap();
        // Exporting an empty ANTHROPIC_API_KEY would look authenticated and
        // fail at request time, which is the worst of both.
        assert!(credential_env(&d, Harness::Claude).is_empty());

        store_api_key(&d, "sk-y").unwrap();
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

    #[test]
    fn clearing_removes_the_key_and_is_idempotent() {
        let d = tmp("clear");
        store_api_key(&d, "sk-z").unwrap();
        clear_api_key(&d).unwrap();
        assert!(read_api_key(&d).is_none());
        // Clearing again must not error: stop/restart paths call it blindly.
        clear_api_key(&d).unwrap();
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
