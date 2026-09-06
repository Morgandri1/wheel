//! Environment configuration.
//!
//! The single most important property of this module: **unsafe configurations refuse to boot.**
//! It is better to fail loudly on startup than to serve traffic with a development authentication
//! bypass quietly enabled in production.

use crate::crypto::Secret;
use anyhow::{anyhow, bail, Context, Result};
use base64::Engine as _;

/// Which identity provider verifies session tokens.
///
/// The two modes end at the same `VerifiedUser`, so everything downstream — the ownership
/// extractor above all — is unaware of which one ran. Swapping providers is configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthMode {
    /// Built-in: users and passwords in our own database, HS256 sessions we issue.
    Local,
    /// External: RS256 tokens verified against a provider's JWKS.
    Jwks,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Env {
    Dev,
    Prod,
}

impl Env {
    pub fn is_dev(self) -> bool {
        self == Env::Dev
    }
}

#[derive(Clone)]
pub struct Config {
    pub env: Env,
    pub bind_addr: String,
    /// Where the API keeps projects, users and sessions. `postgres://…` in production;
    /// `sqlite://…` for a local install, where there is nothing to install and one writer.
    pub database_url: String,

    // Auth
    pub clerk_jwks_url: String,
    pub clerk_issuer: String,
    /// Optional authorized-party allowlist. When non-empty, `azp` must be one of these.
    pub clerk_azp: Vec<String>,
    /// HS256 shared secret for local testing. Only ever populated when `env == Dev`.
    pub dev_secret: Option<String>,
    pub auth_mode: AuthMode,
    /// Signing key for locally issued sessions. Only meaningful when `auth_mode == Local`.
    pub session_secret: Secret,

    // Crypto
    pub master_key: [u8; 32],

    // wheel-host: the single machine that owns every project sandbox. The API never talks to a
    // container runtime or to an engine directly — everything goes through here.
    pub host_url: String,
    pub host_secret: Secret,
    pub engine_port: u16,

    // Limits
    pub public_base_url: String,
    pub max_projects_per_user: i64,
    pub ingress_rate_per_min: u32,
    pub ingress_body_limit_bytes: usize,
    pub proxy_timeout_secs: u64,
    /// How long to wait for a TCP connection to the host before calling it unreachable.
    pub host_connect_timeout_secs: u64,
}

/// Derive the session signing key from the master key, with domain separation.
///
/// The label keeps this key distinct from every other use of the master key, so a weakness in one
/// does not become a weakness in the other. Changing the label invalidates every issued session,
/// which is a deliberate lever: it revokes everything at once without rotating the master key and
/// re-encrypting every project secret.
fn derive_session_key(master_key: &[u8; 32]) -> String {
    use hmac::{Hmac, Mac};
    let mut mac = <Hmac<sha2::Sha256> as Mac>::new_from_slice(master_key)
        .expect("hmac accepts any key length");
    mac.update(b"wheel/session-signing-key/v1");
    hex::encode(mac.finalize().into_bytes())
}

fn var(key: &str) -> Result<String> {
    std::env::var(key).with_context(|| format!("required environment variable {key} is not set"))
}

fn var_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

fn parse_or<T: std::str::FromStr>(key: &str, default: T) -> Result<T> {
    match std::env::var(key) {
        Ok(v) => v.parse::<T>().map_err(|_| {
            anyhow!(
                "environment variable {key} is not a valid {}",
                std::any::type_name::<T>()
            )
        }),
        Err(_) => Ok(default),
    }
}

impl Config {
    pub fn from_env() -> Result<Self> {
        // Default to Prod when unset. Fail-closed: an unset or misspelled WHEEL_ENV must never
        // silently grant development privileges.
        let raw_env = var_or("WHEEL_ENV", "prod");
        let env = match raw_env.as_str() {
            "dev" => Env::Dev,
            "prod" => Env::Prod,
            other => bail!("WHEEL_ENV must be exactly \"dev\" or \"prod\", got {other:?}"),
        };

        // --- The dev-bypass interlock ---------------------------------------------------------
        // AUTH_DEV_SECRET enables HS256 tokens, which anyone holding the secret can mint. It is a
        // complete authentication bypass by design, for local testing. If it is present while we
        // are not explicitly in dev, that is either a misconfiguration or an attack, and the only
        // safe response is to not start.
        let dev_secret = std::env::var("AUTH_DEV_SECRET")
            .ok()
            .filter(|s| !s.is_empty());
        let dev_secret = match (env, dev_secret) {
            (Env::Prod, Some(_)) => bail!(
                "AUTH_DEV_SECRET is set but WHEEL_ENV is not \"dev\". This would enable HS256 \
                 token forgery against a production deployment. Refusing to boot."
            ),
            (Env::Dev, Some(s)) => {
                tracing::warn!(
                    "AUTH_DEV_SECRET is enabled: unsigned-by-Clerk HS256 tokens will be accepted. \
                     This must never be reachable from the internet."
                );
                Some(s)
            }
            (_, None) => None,
        };

        // Which provider verifies tokens. Unset in prod is refused rather than defaulted: guessing
        // wrong means either rejecting every real user or accepting tokens from the wrong issuer,
        // and both are worse than not starting.
        let auth_mode = match std::env::var("AUTH_MODE").ok().as_deref() {
            Some("local") => AuthMode::Local,
            Some("jwks") => AuthMode::Jwks,
            Some(other) => bail!("AUTH_MODE must be \"local\" or \"jwks\", got {other:?}"),
            None if env == Env::Dev => AuthMode::Local,
            None => bail!("AUTH_MODE must be set in production (\"local\" or \"jwks\")"),
        };

        let master_key = {
            let raw = var("API_MASTER_KEY")?;
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(raw.trim())
                .context("API_MASTER_KEY must be valid base64")?;
            let len = bytes.len();
            <[u8; 32]>::try_from(bytes.as_slice())
                .map_err(|_| anyhow!("API_MASTER_KEY must decode to exactly 32 bytes, got {len}"))?
        };

        // A session secret is what stands between anyone and every account, so a short or absent
        // one is refused rather than padded or derived silently.
        // Session signing key.
        //
        // An explicit SESSION_SECRET wins, because it can be rotated independently. When it is
        // absent the key is *derived* from API_MASTER_KEY rather than reusing it: the master key
        // already encrypts project secrets, and using one key for two purposes means a weakness in
        // either compromises both. HMAC with a fixed label gives domain separation for free, so
        // the session key and the encryption key are unrelated even though one produces the other.
        let session_secret = match auth_mode {
            AuthMode::Local => match std::env::var("SESSION_SECRET")
                .ok()
                .filter(|s| !s.is_empty())
            {
                Some(s) => {
                    if s.len() < 32 {
                        bail!("SESSION_SECRET must be at least 32 characters");
                    }
                    Secret::new(s)
                }
                None => Secret::new(derive_session_key(&master_key)),
            },
            AuthMode::Jwks => Secret::new(String::new()),
        };

        let clerk_azp = var_or("CLERK_AZP", "")
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        let cfg = Config {
            env,
            bind_addr: var_or("BIND_ADDR", "0.0.0.0:8080"),
            // STORE first, DATABASE_URL second. The new name says what it is — Postgres is no
            // longer the only answer — and the old one keeps every existing deployment booting.
            database_url: match std::env::var("STORE") {
                Ok(s) if !s.trim().is_empty() => s,
                _ => var("DATABASE_URL")
                    .context("set STORE (postgres://… or sqlite://…), or DATABASE_URL")?,
            },
            // Only meaningful under AUTH_MODE=jwks; blank is fine and expected under local.
            clerk_jwks_url: var_or("CLERK_JWKS_URL", ""),
            clerk_issuer: var_or("CLERK_ISSUER", ""),
            clerk_azp,
            dev_secret,
            auth_mode,
            session_secret,
            master_key,
            host_url: var("WHEEL_HOST_URL")?.trim_end_matches('/').to_string(),
            host_secret: Secret::new(var("WHEEL_HOST_SECRET")?),
            engine_port: parse_or("ENGINE_PORT", 7000u16)?,
            public_base_url: var_or("PUBLIC_BASE_URL", "http://localhost:8080"),
            max_projects_per_user: parse_or("MAX_PROJECTS_PER_USER", 20i64)?,
            ingress_rate_per_min: parse_or("INGRESS_RATE_PER_MIN", 60u32)?,
            ingress_body_limit_bytes: parse_or("INGRESS_BODY_LIMIT_BYTES", 5 * 1024 * 1024usize)?,
            proxy_timeout_secs: parse_or("PROXY_TIMEOUT_SECS", 30u64)?,
            host_connect_timeout_secs: parse_or("HOST_CONNECT_TIMEOUT_SECS", 3u64)?,
        };

        if cfg.host_secret.expose().is_empty() {
            bail!("WHEEL_HOST_SECRET must not be empty: it is the only thing authenticating the API to the host");
        }
        if cfg.auth_mode == AuthMode::Jwks
            && (cfg.clerk_jwks_url.trim().is_empty() || cfg.clerk_issuer.trim().is_empty())
        {
            bail!(
                "AUTH_MODE=jwks requires CLERK_JWKS_URL and CLERK_ISSUER to be set to real values. \
                 A placeholder that looks like configuration is worse than a missing one: it boots, \
                 and then rejects every token for a reason nobody can see."
            );
        }
        if cfg.auth_mode == AuthMode::Jwks && cfg.clerk_issuer.is_empty() {
            bail!("CLERK_ISSUER must not be empty: it is what pins tokens to our tenant");
        }
        if cfg.env == Env::Prod && cfg.auth_mode == AuthMode::Jwks {
            // ADVERSARY 017: an identity provider we do not control is a provider that can mint any
            // `sub`. A mock issuer on loopback is the dev shortcut that must never survive a deploy:
            // it does not fail — it authenticates everyone, as anyone.
            reject_local_identity_provider("CLERK_JWKS_URL", &cfg.clerk_jwks_url)?;
            reject_local_identity_provider("CLERK_ISSUER", &cfg.clerk_issuer)?;
        }
        Ok(cfg)
    }

    /// Base URL for this project's engine control plane, as reached through the host.
    pub fn host_engine_url(&self, project_id: &uuid::Uuid) -> String {
        format!("{}/host/v1/projects/{}/engine", self.host_url, project_id)
    }

    /// Base URL for this project's public ingress, as reached through the host.
    pub fn host_ingress_url(&self, project_id: &uuid::Uuid) -> String {
        format!("{}/host/v1/projects/{}/ingress", self.host_url, project_id)
    }
}

/// The bare host from the part of a URL after the scheme: no userinfo, no port, and no brackets
/// around an IPv6 literal.
fn host_of(after_scheme: &str) -> &str {
    let authority = after_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("")
        .rsplit('@')
        .next()
        .unwrap_or("");

    // An IPv6 literal is bracketed precisely because it is full of colons; stripping a "port" from
    // it by splitting on the last colon turns [::1] into ":".
    match authority.strip_prefix('[') {
        Some(rest) => rest.split(']').next().unwrap_or(""),
        None => authority.split(':').next().unwrap_or(""),
    }
}

/// True for hosts that only this machine or this private network can reach.
///
/// Names and literals only — see `reject_local_identity_provider` for why there is no DNS here.
fn is_unroutable_host(host: &str) -> bool {
    const LOCAL_SUFFIXES: [&str; 3] = [".localhost", ".local", ".internal"];
    const LOCAL_PREFIXES: [&str; 5] = ["127.", "10.", "192.168.", "169.254.", "0."];

    if host.is_empty() || host == "localhost" || host == "::1" {
        return true;
    }
    if LOCAL_SUFFIXES.iter().any(|s| host.ends_with(s))
        || LOCAL_PREFIXES.iter().any(|p| host.starts_with(p))
    {
        return true;
    }
    // Unique-local IPv6 (fc00::/7).
    if host.starts_with("fc") || host.starts_with("fd") {
        return true;
    }
    // 172.16.0.0/12 — the second octet decides, so "172.15." and "172.32." are public.
    host.strip_prefix("172.")
        .and_then(|rest| rest.split('.').next())
        .and_then(|octet| octet.parse::<u8>().ok())
        .is_some_and(|octet| (16..32).contains(&octet))
}

/// Refuse an identity-provider URL that is plaintext or points somewhere only this machine can
/// reach — the shape of a dev stub, a stand-in, or a rebinding target.
///
/// Checked on the literal host, without DNS: a name that resolves to loopback today may not
/// tomorrow, and boot is not the place to trust a resolver. This is a tripwire for the obvious
/// mistake, not a substitute for pointing at the real issuer.
fn reject_local_identity_provider(var: &str, raw: &str) -> Result<()> {
    let value = raw.trim();
    let Some(rest) = value
        .strip_prefix("https://")
        .or_else(|| value.strip_prefix("http://"))
    else {
        bail!("{var} must be an absolute https:// URL in production, got {value:?}");
    };
    if value.starts_with("http://") {
        bail!(
            "{var} must be https:// in production, got {value:?}. Plaintext to an identity \
             provider means anyone on the path chooses who our users are."
        );
    }

    let bare = host_of(rest);
    let lower = bare.to_ascii_lowercase();

    if is_unroutable_host(&lower) {
        bail!(
            "{var} points at {bare:?}, which only this machine can reach — that is a stub issuer, \
             and a stub issuer in production authenticates everyone as anyone (ADVERSARY 017)."
        );
    }
    Ok(())
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Every secret-bearing field is elided. `Config` is formatted at boot and in test
        // assertions, and a derived impl would put the master key in the logs.
        f.debug_struct("Config")
            .field("env", &self.env)
            .field("bind_addr", &self.bind_addr)
            .field("clerk_issuer", &self.clerk_issuer)
            .field("clerk_jwks_url", &self.clerk_jwks_url)
            .field("clerk_azp", &self.clerk_azp)
            .field("host_url", &self.host_url)
            .field("host_secret", &"<redacted>")
            .field("master_key", &"<redacted>")
            .field(
                "dev_secret",
                &self.dev_secret.as_ref().map(|_| "<redacted>"),
            )
            .field("database_url", &"<redacted>")
            .field("max_projects_per_user", &self.max_projects_per_user)
            .field("ingress_rate_per_min", &self.ingress_rate_per_min)
            .finish()
    }
}
