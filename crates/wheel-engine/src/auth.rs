// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

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
    read_token(config_dir).is_some() || has_native_login_store(config_dir, harness)
}

/// Has the harness's OWN login (`claude auth login` paste-code, `codex
/// login` device-code) written its own credential store into this node's
/// config dir -- as opposed to a credential entered through `auth/complete`
/// into [`TOKEN_FILE`]? The two are different surfaces (module doc); this
/// checks only the harness's own store, which is what [`is_oauth_shaped`]
/// needs to tell apart from a stored API key.
fn has_native_login_store(config_dir: &Path, harness: Harness) -> bool {
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

/// Whether this node's currently-visible credential is OAuth-shaped rather
/// than an API key, across every surface it can arrive on except a wired
/// vault (vault values are gated separately, at `PUT` time -- API's half of
/// docs/proposals/wheel-harness-auth.md).
///
/// Backs the `api-key-only` policy's spawn gate AND its periodic re-check
/// while running (the proposal's "PM ruling: mid-flow enforcement window" --
/// an agent can run `claude auth login`/`codex login` itself mid-turn, so the
/// same check has to run more than once).
///
/// Claude has a bearer token to classify: either the per-node [`TOKEN_FILE`]
/// holds one ([`classify_token`] already tells the two kinds apart by the
/// `sk-ant-oat` prefix), or the harness's own native store does
/// ([`oauth_token_from_store`] finding anything at all IS an OAuth
/// credential -- that function only ever looks in `claude auth login`'s own
/// files). Codex has no such token: per [`token_env`]'s own reasoning, its
/// OAuth is a *session* written to `auth.json`, not a string this engine can
/// classify -- so for codex, a native login session existing at all, with no
/// stored API key to prefer instead, IS the signal.
pub fn is_oauth_shaped(config_dir: &Path, harness: Harness) -> bool {
    match harness {
        Harness::Claude => {
            let via_token_file = stored_token_kind(config_dir, harness)
                .is_some_and(|k| k == CredentialKind::OauthToken);
            via_token_file || oauth_token_from_store(config_dir, None).is_ok()
        }
        Harness::Codex => {
            read_token(config_dir).is_none() && has_native_login_store(config_dir, harness)
        }
    }
}

/// A credential recovered from the harness's own store, so it can be handed to
/// other agents through a vault.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredOauth {
    pub token: String,
    /// Milliseconds since the epoch, when the store says so. `None` means the
    /// store did not say -- NOT that the token is durable.
    pub expires_at: Option<i64>,
    /// Set only by a route that received the credential as a kind that is
    /// durable by definition (`setup_token`, `api_key`). Nothing read out of a
    /// store sets it: a missing expiry there is a failure to read.
    pub durable: bool,
}

impl StoredOauth {
    pub fn durable(token: impl Into<String>) -> Self {
        Self {
            token: token.into(),
            expires_at: None,
            durable: true,
        }
    }

    /// Durable only when the caller SAID so. The caller has to tell a durable
    /// credential from a session one before copying it into a vault five other
    /// agents read, and "the store did not mention an expiry" is not evidence
    /// of either.
    pub fn is_long_lived(&self) -> bool {
        self.durable && self.expires_at.is_none()
    }
}

/// A directory removed when this is dropped, on every path out — an early
/// `?` and a cancelled future included — so a captured or renewed login is
/// never left on disk anywhere but the (encrypted) vault.
pub struct ScratchDir(pub PathBuf);

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The CLI's own credential store inside a config dir it was pointed at.
const CLI_STORE_FILE: &str = ".credentials.json";
/// The CLI's own config file, which is where it records whose login it is.
const CLI_CONFIG_FILE: &str = ".claude.json";
const MAX_STORE_BYTES: u64 = 64 * 1024;
const MAX_CONFIG_BYTES: u64 = 4 * 1024 * 1024;
/// No refreshed login is trusted to last longer than this. `claude
/// setup-token` mints a one-year token; anything past that is not a refresh.
const MAX_HORIZON_MS: i64 = 400 * 24 * 60 * 60 * 1000;

/// Whose login a session is, as far as the CLI recorded it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Identity {
    pub account_uuid: Option<String>,
    pub organization_uuid: Option<String>,
}

/// A refreshable Claude login: the CLI's `claudeAiOauth` object kept WHOLE,
/// refresh token included, plus the account it belongs to.
///
/// Kept as the CLI's own map rather than a struct of the fields we know, so a
/// field the CLI adds is carried rather than silently dropped on a round trip.
#[derive(Debug, Clone, PartialEq)]
pub struct OauthSession {
    oauth: serde_json::Map<String, serde_json::Value>,
    account: Identity,
}

impl OauthSession {
    fn text(&self, field: &str) -> Option<&str> {
        self.oauth
            .get(field)
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
    }

    pub fn access_token(&self) -> Option<&str> {
        self.text("accessToken")
    }

    pub fn refresh_token(&self) -> Option<&str> {
        self.text("refreshToken")
    }

    /// Milliseconds since the epoch, as the CLI records it.
    pub fn expires_at(&self) -> Option<i64> {
        self.oauth.get("expiresAt").and_then(|v| v.as_i64())
    }

    pub fn scopes(&self) -> Vec<String> {
        self.oauth
            .get("scopes")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|s| s.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn account(&self) -> &Identity {
        &self.account
    }

    /// Everything the engine needs to renew this login without a person: a
    /// token to run on, a refresh token, the scopes it was issued with (the
    /// CLI refuses to refresh without them), and a deadline to renew before.
    pub fn is_refreshable(&self) -> bool {
        self.access_token().is_some()
            && self.refresh_token().is_some()
            && !self.scopes().is_empty()
            && self.expires_at().is_some()
    }

    /// What a vault stores: the CLI's object, and whose it is.
    pub fn to_vault_value(&self) -> String {
        let mut doc = serde_json::json!({ "claudeAiOauth": self.oauth });
        let mut account = serde_json::Map::new();
        if let Some(a) = &self.account.account_uuid {
            account.insert("accountUuid".into(), a.clone().into());
        }
        if let Some(o) = &self.account.organization_uuid {
            account.insert("organizationUuid".into(), o.clone().into());
        }
        if !account.is_empty() {
            doc["oauthAccount"] = serde_json::Value::Object(account);
        }
        doc.to_string()
    }

    pub fn from_vault_value(raw: &str) -> Result<Self> {
        let doc: serde_json::Value =
            serde_json::from_str(raw).context("the stored session is not JSON")?;
        let oauth = doc
            .get("claudeAiOauth")
            .and_then(|v| v.as_object())
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("the stored session has no claudeAiOauth object"))?;
        Ok(Self {
            oauth,
            account: identity_in(&doc),
        })
    }

    /// The same login as a bare access token, for the paths that still hand
    /// one out. Never durable: it came out of a store.
    pub fn as_stored(&self) -> Option<StoredOauth> {
        Some(StoredOauth {
            token: self.access_token()?.to_string(),
            expires_at: self.expires_at(),
            durable: false,
        })
    }

    /// Fill what a refresh legitimately leaves out from the login it renewed.
    ///
    /// The CLI records scopes from the token response and account identity
    /// from a profile fetch; either can be absent without the login being any
    /// less the same one, and dropping them would make the NEXT refresh
    /// impossible (no scopes) or unverifiable (no identity).
    pub fn carry_forward(&mut self, prev: &OauthSession) {
        if self.scopes().is_empty() && !prev.scopes().is_empty() {
            self.oauth
                .insert("scopes".into(), serde_json::json!(prev.scopes()));
        }
        if self.account.account_uuid.is_none() {
            self.account.account_uuid = prev.account.account_uuid.clone();
        }
        if self.account.organization_uuid.is_none() {
            self.account.organization_uuid = prev.account.organization_uuid.clone();
        }
    }

    #[cfg(test)]
    pub fn for_tests(
        access: &str,
        refresh: &str,
        expires_at: i64,
        scopes: &[&str],
        account: Option<&str>,
    ) -> Self {
        let oauth = serde_json::json!({
            "accessToken": access,
            "refreshToken": refresh,
            "expiresAt": expires_at,
            "scopes": scopes,
            "subscriptionType": "max",
        });
        Self {
            oauth: oauth.as_object().unwrap().clone(),
            account: Identity {
                account_uuid: account.map(str::to_string),
                organization_uuid: account.map(|a| format!("org-of-{a}")),
            },
        }
    }
}

fn identity_in(doc: &serde_json::Value) -> Identity {
    let field = |k: &str| {
        doc.get("oauthAccount")
            .and_then(|a| a.get(k))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    Identity {
        account_uuid: field("accountUuid"),
        organization_uuid: field("organizationUuid"),
    }
}

/// Read a file only if it is a regular file, opened without following a
/// symlink, and no bigger than `cap`. `None` when it is not there.
///
/// O_NOFOLLOW at open rather than a metadata check first: checking and then
/// opening leaves a window in which the path can become a link to another
/// node's store.
fn read_regular(path: &Path, cap: u64) -> Result<Option<String>> {
    use std::{io::Read, os::unix::fs::OpenOptionsExt};
    let file = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
    {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("opening {}", path.display())),
    };
    let meta = file.metadata()?;
    if !meta.is_file() {
        bail!("{} is not a regular file", path.display());
    }
    if meta.len() > cap {
        bail!(
            "{} is larger than a credential store can be",
            path.display()
        );
    }
    let mut out = String::new();
    file.take(cap).read_to_string(&mut out)?;
    Ok(Some(out))
}

/// The login the CLI wrote into `dir`, read strictly: its own store file and
/// nothing else, a regular file, and exactly the `claudeAiOauth` object.
///
/// Only ever pointed at a directory the ENGINE created for one login or one
/// refresh — never at an agent's HOME, which untrusted code can write.
pub fn read_session(dir: &Path) -> Result<OauthSession> {
    let raw = read_regular(&dir.join(CLI_STORE_FILE), MAX_STORE_BYTES)?
        .ok_or_else(|| anyhow::anyhow!("the CLI wrote no credential store"))?;
    let doc: serde_json::Value =
        serde_json::from_str(&raw).context("the CLI's credential store is not JSON")?;
    let oauth = doc
        .get("claudeAiOauth")
        .and_then(|v| v.as_object())
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("the CLI's credential store holds no claude.ai login"))?;
    let account = read_regular(&dir.join(CLI_CONFIG_FILE), MAX_CONFIG_BYTES)?
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        .map(|doc| identity_in(&doc))
        .unwrap_or_default();
    Ok(OauthSession { oauth, account })
}

/// Why a refreshed login was not accepted as the successor of the one it
/// claims to renew.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RefreshRejected {
    #[error("it carries no access token")]
    NoAccessToken,
    #[error("its access token is the one it was meant to replace")]
    SameAccessToken,
    #[error("it carries no refresh token")]
    NoRefreshToken,
    #[error("it carries no expiry")]
    NoExpiry,
    #[error("it does not expire later than the login it replaces")]
    NotNewer,
    #[error("its expiry is not a plausible one")]
    Implausible,
    #[error("it asks for scopes the original login did not have")]
    ScopeEscalation,
    #[error("it belongs to a different account")]
    AccountChanged,
}

/// The write-back gate: is `next` a plausible renewal of `prev`, the login a
/// vault is about to hand to every agent that reads it?
///
/// Pure, so each refusal is testable on its own. A missing expiry is a
/// failure to read, never a promotion; an identity is compared wherever
/// BOTH sides expose it.
pub fn check_refresh(
    prev: &OauthSession,
    next: &OauthSession,
    now_ms: i64,
) -> std::result::Result<(), RefreshRejected> {
    use RefreshRejected as R;
    let access = next.access_token().ok_or(R::NoAccessToken)?;
    if prev.access_token() == Some(access) {
        return Err(R::SameAccessToken);
    }
    next.refresh_token().ok_or(R::NoRefreshToken)?;
    let expires = next.expires_at().ok_or(R::NoExpiry)?;
    if prev.expires_at().is_some_and(|p| expires <= p) {
        return Err(R::NotNewer);
    }
    if expires <= now_ms || expires > now_ms.saturating_add(MAX_HORIZON_MS) {
        return Err(R::Implausible);
    }
    let allowed = prev.scopes();
    if next.scopes().iter().any(|s| !allowed.contains(s)) {
        return Err(R::ScopeEscalation);
    }
    let differs =
        |a: &Option<String>, b: &Option<String>| matches!((a, b), (Some(x), Some(y)) if x != y);
    if differs(&prev.account.account_uuid, &next.account.account_uuid)
        || differs(
            &prev.account.organization_uuid,
            &next.account.organization_uuid,
        )
    {
        return Err(R::AccountChanged);
    }
    Ok(())
}

/// Put a login the engine captured into a node's own config dir, as the
/// CLI's own store, so that node signs in with it and refreshes it itself.
///
/// Written beside the target and renamed over it: renaming replaces a symlink
/// an agent planted at that path instead of writing through it into whatever
/// it points at.
pub fn install_store(from: &Path, node_dir: &Path) -> Result<()> {
    std::fs::create_dir_all(node_dir)?;
    set_mode(node_dir, 0o700)?;
    let store = read_regular(&from.join(CLI_STORE_FILE), MAX_STORE_BYTES)?
        .ok_or_else(|| anyhow::anyhow!("the login left no credential store to install"))?;
    replace_secret(&node_dir.join(CLI_STORE_FILE), &store)?;
    let config_target = node_dir.join(CLI_CONFIG_FILE);
    if std::fs::symlink_metadata(&config_target).is_err() {
        if let Some(config) = read_regular(&from.join(CLI_CONFIG_FILE), MAX_CONFIG_BYTES)? {
            replace_secret(&config_target, &config)?;
        }
    }
    Ok(())
}

fn replace_secret(target: &Path, contents: &str) -> Result<()> {
    let tmp = target.with_extension("wheel-tmp");
    let _ = std::fs::remove_file(&tmp);
    {
        use std::{io::Write, os::unix::fs::OpenOptionsExt};
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&tmp)
            .with_context(|| format!("writing {}", tmp.display()))?;
        f.write_all(contents.as_bytes())?;
        f.flush()?;
    }
    std::fs::rename(&tmp, target).with_context(|| format!("installing {}", target.display()))?;
    Ok(())
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
                            durable: false,
                        });
                    }
                }
                // A token by its marker, wherever it sits.
                if let Some(t) = val.as_str().filter(|t| t.starts_with(OAUTH_TOKEN_PREFIX)) {
                    return Some(StoredOauth {
                        token: t.to_string(),
                        expires_at: expiry_in(map),
                        durable: false,
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

    /// A stored API key is not OAuth-shaped, on either harness -- the whole
    /// point of `is_oauth_shaped` is telling this apart from the next test.
    #[test]
    fn a_stored_api_key_is_not_oauth_shaped() {
        let d = tmp("shape-apikey");
        assert!(!is_oauth_shaped(&d, Harness::Claude));
        store_token(&d, "sk-ant-api03-xyz", Harness::Claude).unwrap();
        assert!(!is_oauth_shaped(&d, Harness::Claude));
        std::fs::remove_dir_all(&d).ok();

        let d = tmp("shape-apikey-codex");
        assert!(!is_oauth_shaped(&d, Harness::Codex));
        store_token(&d, "sk-proj-xyz", Harness::Codex).unwrap();
        assert!(!is_oauth_shaped(&d, Harness::Codex));
        std::fs::remove_dir_all(&d).ok();
    }

    /// A `setup_token`/OAuth value in the per-node [`TOKEN_FILE`] is the
    /// first surface `is_oauth_shaped` has to catch -- classified from the
    /// value itself, same as `credential_env`.
    #[test]
    fn a_stored_oauth_token_is_oauth_shaped() {
        let d = tmp("shape-oat");
        store_token(&d, "sk-ant-oat01-abc", Harness::Claude).unwrap();
        assert!(is_oauth_shaped(&d, Harness::Claude));
        std::fs::remove_dir_all(&d).ok();
    }

    /// The harness's OWN login store is the surface an agent can write to
    /// itself, mid-turn, with no API route involved -- the gap
    /// wheel-harness-auth.md's spawn gate and periodic re-check both exist
    /// to close.
    #[test]
    fn a_native_login_store_is_oauth_shaped_even_with_no_wheel_token_file() {
        let d = tmp("shape-native");
        std::fs::create_dir_all(&d).unwrap();
        assert!(!is_oauth_shaped(&d, Harness::Claude));
        std::fs::write(
            d.join(".credentials.json"),
            r#"{"accessToken":"sk-ant-oat01-selfprovisioned"}"#,
        )
        .unwrap();
        assert!(is_oauth_shaped(&d, Harness::Claude));
        std::fs::remove_dir_all(&d).ok();
    }

    /// Codex has no bearer token to classify -- a login SESSION existing at
    /// all, with no `CODEX_API_KEY` stored to prefer instead, is itself the
    /// OAuth signal (auth.rs module doc, and `token_env`'s own reasoning).
    #[test]
    fn a_codex_login_session_is_oauth_shaped_only_without_a_stored_api_key() {
        let d = tmp("shape-codex-session");
        std::fs::create_dir_all(&d).unwrap();
        assert!(!is_oauth_shaped(&d, Harness::Codex));

        std::fs::write(d.join("auth.json"), r#"{"tokens":{}}"#).unwrap();
        assert!(
            is_oauth_shaped(&d, Harness::Codex),
            "a login session with no stored API key is OAuth-shaped"
        );

        // An operator who then stores an API key on this node fixes it going
        // forward without deleting the stale session file -- the presence of
        // a real API key must win, not the leftover session.
        store_token(&d, "sk-proj-real", Harness::Codex).unwrap();
        assert!(
            !is_oauth_shaped(&d, Harness::Codex),
            "a stored API key must outrank a leftover session file"
        );
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

    /// The inversion the handoff asked for. A store entry with an `oat` token
    /// and no `expiresAt` used to be promoted to "durable" and vaulted for
    /// every peer with no warning. A field the engine failed to read is not a
    /// promise; only a route that received a `setup_token` can say durable.
    #[test]
    fn a_store_entry_never_certifies_its_own_durability() {
        let d = dir("durable");
        std::fs::write(
            d.join(".credentials.json"),
            r#"{"accessToken":"sk-ant-oat01-durable"}"#,
        )
        .unwrap();
        let found = oauth_token_from_store(&d, None).unwrap();
        assert_eq!(found.expires_at, None);
        assert!(
            !found.is_long_lived(),
            "no expiry in a store is unknown, not durable"
        );
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
mod session_tests {
    use super::*;

    const NOW: i64 = 1_800_000_000_000;
    const HOUR: i64 = 3_600_000;
    const SCOPES: &[&str] = &["user:inference", "user:profile"];

    fn prev() -> OauthSession {
        OauthSession::for_tests(
            "sk-ant-oat01-old",
            "sk-ant-ort01-old",
            NOW + HOUR,
            SCOPES,
            Some("acct-A"),
        )
    }

    fn next() -> OauthSession {
        OauthSession::for_tests(
            "sk-ant-oat01-new",
            "sk-ant-ort01-new",
            NOW + 8 * HOUR,
            SCOPES,
            Some("acct-A"),
        )
    }

    fn with(mut s: OauthSession, field: &str, v: serde_json::Value) -> OauthSession {
        if v.is_null() {
            s.oauth.remove(field);
        } else {
            s.oauth.insert(field.into(), v);
        }
        s
    }

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "wheel-session-{name}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn a_plausible_renewal_of_the_same_login_passes_the_gate() {
        assert_eq!(check_refresh(&prev(), &next(), NOW), Ok(()));
        // A server that does not rotate refresh tokens is still renewing.
        let same_refresh = with(next(), "refreshToken", "sk-ant-ort01-old".into());
        assert_eq!(check_refresh(&prev(), &same_refresh, NOW), Ok(()));
        // Identity the refresher could not read is not a different account.
        let mut unknown = next();
        unknown.account = Identity::default();
        assert_eq!(check_refresh(&prev(), &unknown, NOW), Ok(()));
    }

    /// Every way a write-back can fail to be a renewal of the login a vault is
    /// about to hand to every peer. Each arm is the gate's only line of
    /// defence against that shape; the list IS the spec.
    #[test]
    fn the_write_back_gate_refuses_everything_that_is_not_a_renewal() {
        use RefreshRejected as R;
        let cases: Vec<(&str, OauthSession, R)> = vec![
            (
                "no access token",
                with(next(), "accessToken", serde_json::Value::Null),
                R::NoAccessToken,
            ),
            (
                "the same access token",
                with(next(), "accessToken", "sk-ant-oat01-old".into()),
                R::SameAccessToken,
            ),
            (
                "no refresh token",
                with(next(), "refreshToken", "".into()),
                R::NoRefreshToken,
            ),
            (
                "no expiry: a failure to read, not a promotion",
                with(next(), "expiresAt", serde_json::Value::Null),
                R::NoExpiry,
            ),
            (
                "an expiry no later than the old one",
                with(next(), "expiresAt", (NOW + HOUR).into()),
                R::NotNewer,
            ),
            (
                "an expiry a year and more out",
                with(next(), "expiresAt", (NOW + 401 * 24 * HOUR).into()),
                R::Implausible,
            ),
            (
                "a scope the original never had",
                with(
                    next(),
                    "scopes",
                    serde_json::json!(["user:inference", "org:admin"]),
                ),
                R::ScopeEscalation,
            ),
            (
                "another account",
                OauthSession::for_tests(
                    "sk-ant-oat01-evil",
                    "sk-ant-ort01-evil",
                    NOW + 8 * HOUR,
                    SCOPES,
                    Some("acct-EVIL"),
                ),
                R::AccountChanged,
            ),
        ];
        for (what, candidate, want) in cases {
            assert_eq!(
                check_refresh(&prev(), &candidate, NOW),
                Err(want),
                "{what} must be refused"
            );
        }

        // The same account in another organisation is a different login too.
        let mut other_org = next();
        other_org.account.organization_uuid = Some("org-EVIL".into());
        assert_eq!(
            check_refresh(&prev(), &other_org, NOW),
            Err(R::AccountChanged)
        );
        // ...and an expiry already in the past renews nothing.
        let lapsed = with(prev(), "expiresAt", (NOW - 2 * HOUR).into());
        let stale_next = with(next(), "expiresAt", (NOW - HOUR).into());
        assert_eq!(
            check_refresh(&lapsed, &stale_next, NOW),
            Err(R::Implausible)
        );
    }

    #[test]
    fn a_vaulted_login_round_trips_whole_including_fields_we_do_not_know() {
        let s = with(next(), "someFieldAddedLater", serde_json::json!({"x": 1}));
        let back = OauthSession::from_vault_value(&s.to_vault_value()).unwrap();
        assert_eq!(back, s);
        assert_eq!(back.refresh_token(), Some("sk-ant-ort01-new"));
        assert_eq!(back.account().account_uuid.as_deref(), Some("acct-A"));
        assert!(OauthSession::from_vault_value("sk-ant-oat01-bare").is_err());
        assert!(OauthSession::from_vault_value(r#"{"other":{}}"#).is_err());
    }

    /// Scopes and identity are allowed to be missing from a renewal, but not
    /// to be LOST by it: without scopes the next renewal is impossible, and
    /// without identity it is unverifiable.
    #[test]
    fn a_renewal_keeps_what_it_left_out_from_the_login_it_renewed() {
        let mut thin = with(next(), "scopes", serde_json::Value::Null);
        thin.account = Identity::default();
        thin.carry_forward(&prev());
        assert_eq!(thin.scopes(), SCOPES);
        assert_eq!(thin.account(), prev().account());

        // What the renewal DID say wins.
        let mut said = next();
        said.account.account_uuid = Some("acct-A".into());
        said.carry_forward(&prev());
        assert_eq!(said.access_token(), Some("sk-ant-oat01-new"));
    }

    #[test]
    fn only_a_login_with_everything_needed_to_renew_it_is_refreshable() {
        assert!(next().is_refreshable());
        for field in ["accessToken", "refreshToken", "scopes", "expiresAt"] {
            assert!(
                !with(next(), field, serde_json::Value::Null).is_refreshable(),
                "without {field} it cannot be renewed"
            );
        }
        let as_stored = next().as_stored().unwrap();
        assert_eq!(as_stored.token, "sk-ant-oat01-new");
        assert!(
            !as_stored.is_long_lived(),
            "a store entry is never durable by itself"
        );
    }

    fn write_store(d: &Path, access: &str, account: &str) {
        std::fs::write(
            d.join(".credentials.json"),
            serde_json::json!({"claudeAiOauth": {
                "accessToken": access, "refreshToken": "sk-ant-ort01-x",
                "expiresAt": NOW, "scopes": SCOPES }})
            .to_string(),
        )
        .unwrap();
        std::fs::write(
            d.join(".claude.json"),
            serde_json::json!({"oauthAccount": {"accountUuid": account,
                "organizationUuid": format!("org-of-{account}")}})
            .to_string(),
        )
        .unwrap();
    }

    #[test]
    fn the_cli_store_is_read_with_the_identity_the_cli_recorded() {
        let d = dir("read");
        write_store(&d, "sk-ant-oat01-mine", "acct-A");
        let s = read_session(&d).unwrap();
        assert_eq!(s.access_token(), Some("sk-ant-oat01-mine"));
        assert_eq!(s.account().account_uuid.as_deref(), Some("acct-A"));
        assert_eq!(
            s.account().organization_uuid.as_deref(),
            Some("org-of-acct-A")
        );
        std::fs::remove_dir_all(&d).ok();
    }

    /// Only the CLI's own store path counts. The HOME layout is where a
    /// fallback write COULD land, which is exactly why a strict read must not
    /// look there: it is a second place for something else to be.
    #[test]
    fn the_strict_read_looks_at_the_clis_own_store_and_nowhere_else() {
        let d = dir("home-layout");
        std::fs::create_dir_all(d.join(".claude")).unwrap();
        write_store(&d.join(".claude"), "sk-ant-oat01-elsewhere", "acct-A");
        assert!(read_session(&d).is_err());

        std::fs::write(
            d.join(".credentials.json"),
            r#"{"accessToken":"sk-ant-oat01-flat"}"#,
        )
        .unwrap();
        let err = read_session(&d).unwrap_err().to_string();
        assert!(err.contains("no claude.ai login"), "{err}");
        std::fs::remove_dir_all(&d).ok();
    }

    /// A store that is a symlink is somebody else's store. Following it would
    /// let whoever made the link choose the login that gets read.
    #[test]
    fn a_symlinked_store_is_refused_not_followed() {
        let d = dir("symlink");
        let elsewhere = dir("symlink-target");
        write_store(&elsewhere, "sk-ant-oat01-someone-elses", "acct-EVIL");
        std::os::unix::fs::symlink(
            elsewhere.join(".credentials.json"),
            d.join(".credentials.json"),
        )
        .unwrap();
        assert!(read_session(&d).is_err());
        std::fs::remove_dir_all(&d).ok();
        std::fs::remove_dir_all(&elsewhere).ok();
    }

    /// An agent plants a symlink where its store will go, pointing at another
    /// node's store. Installing must replace the link, not write through it
    /// into the other node's credentials.
    #[test]
    fn installing_a_login_replaces_a_planted_symlink_rather_than_writing_through_it() {
        let from = dir("install-from");
        let node = dir("install-node");
        let victim = dir("install-victim");
        write_store(&from, "sk-ant-oat01-installed", "acct-A");
        std::fs::write(victim.join(".credentials.json"), "victim's own").unwrap();
        std::os::unix::fs::symlink(
            victim.join(".credentials.json"),
            node.join(".credentials.json"),
        )
        .unwrap();

        install_store(&from, &node).unwrap();

        assert_eq!(
            std::fs::read_to_string(victim.join(".credentials.json")).unwrap(),
            "victim's own"
        );
        assert!(!std::fs::symlink_metadata(node.join(".credentials.json"))
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(
            read_session(&node).unwrap().access_token(),
            Some("sk-ant-oat01-installed")
        );
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(node.join(".credentials.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
        for d in [&from, &node, &victim] {
            std::fs::remove_dir_all(d).ok();
        }
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
        assert!(StoredOauth::durable("sk-ant-oat01-durable").is_long_lived());

        let session = StoredOauth {
            token: "sk-ant-oat01-session".into(),
            expires_at: Some(1799999999000),
            durable: false,
        };
        assert!(!session.is_long_lived(), "an expiry disqualifies it");

        // Same token, no expiry, but nobody SAID durable: unknown.
        let unknown = StoredOauth {
            token: "sk-ant-oat01-durable".into(),
            expires_at: None,
            durable: false,
        };
        assert!(!unknown.is_long_lived(), "silence is not a promise");
    }
}
