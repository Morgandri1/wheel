//! Environment configuration.
//!
//! The single most important property of this module: **unsafe configurations refuse to boot.**
//! It is better to fail loudly on startup than to serve traffic with a development authentication
//! bypass quietly enabled in production.

use anyhow::{anyhow, bail, Context, Result};
use crate::crypto::Secret;
use base64::Engine as _;

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
    pub database_url: String,

    // Auth
    pub clerk_jwks_url: String,
    pub clerk_issuer: String,
    /// Optional authorized-party allowlist. When non-empty, `azp` must be one of these.
    pub clerk_azp: Vec<String>,
    /// HS256 shared secret for local testing. Only ever populated when `env == Dev`.
    pub dev_secret: Option<String>,

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
}

fn var(key: &str) -> Result<String> {
    std::env::var(key).with_context(|| format!("required environment variable {key} is not set"))
}

fn var_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

fn parse_or<T: std::str::FromStr>(key: &str, default: T) -> Result<T> {
    match std::env::var(key) {
        Ok(v) => v
            .parse::<T>()
            .map_err(|_| anyhow!("environment variable {key} is not a valid {}", std::any::type_name::<T>())),
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
        let dev_secret = std::env::var("AUTH_DEV_SECRET").ok().filter(|s| !s.is_empty());
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

        let master_key = {
            let raw = var("API_MASTER_KEY")?;
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(raw.trim())
                .context("API_MASTER_KEY must be valid base64")?;
            let len = bytes.len();
            <[u8; 32]>::try_from(bytes.as_slice()).map_err(|_| {
                anyhow!("API_MASTER_KEY must decode to exactly 32 bytes, got {len}")
            })?
        };

        let clerk_azp = var_or("CLERK_AZP", "")
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        let cfg = Config {
            env,
            bind_addr: var_or("BIND_ADDR", "0.0.0.0:8080"),
            database_url: var("DATABASE_URL")?,
            clerk_jwks_url: var("CLERK_JWKS_URL")?,
            clerk_issuer: var("CLERK_ISSUER")?,
            clerk_azp,
            dev_secret,
            master_key,
            host_url: var("WHEEL_HOST_URL")?.trim_end_matches('/').to_string(),
            host_secret: Secret::new(var("WHEEL_HOST_SECRET")?),
            engine_port: parse_or("ENGINE_PORT", 7000u16)?,
            public_base_url: var_or("PUBLIC_BASE_URL", "http://localhost:8080"),
            max_projects_per_user: parse_or("MAX_PROJECTS_PER_USER", 20i64)?,
            ingress_rate_per_min: parse_or("INGRESS_RATE_PER_MIN", 60u32)?,
            ingress_body_limit_bytes: parse_or("INGRESS_BODY_LIMIT_BYTES", 5 * 1024 * 1024usize)?,
            proxy_timeout_secs: parse_or("PROXY_TIMEOUT_SECS", 30u64)?,
        };

        if cfg.host_secret.expose().is_empty() {
            bail!("WHEEL_HOST_SECRET must not be empty: it is the only thing authenticating the API to the host");
        }
        if cfg.clerk_issuer.is_empty() {
            bail!("CLERK_ISSUER must not be empty: it is what pins tokens to our tenant");
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
            .field("dev_secret", &self.dev_secret.as_ref().map(|_| "<redacted>"))
            .field("database_url", &"<redacted>")
            .field("max_projects_per_user", &self.max_projects_per_user)
            .field("ingress_rate_per_min", &self.ingress_rate_per_min)
            .finish()
    }
}
