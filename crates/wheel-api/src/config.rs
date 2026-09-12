// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

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
    /// The deployer's own identity system: a JWT from their issuer, or an assertion from a proxy
    /// they run. See `docs/proposals/external-auth.md` and [`ExternalAuth`].
    External,
}

impl AuthMode {
    /// The wire name, as `/healthz` publishes it and `AUTH_MODE` accepts it.
    pub fn as_str(self) -> &'static str {
        match self {
            AuthMode::Local => "local",
            AuthMode::Jwks => "jwks",
            AuthMode::External => "external",
        }
    }
}

/// Who may create an account through `POST /v1/auth/signup` (local auth only).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignupPolicy {
    /// Anyone who can reach the API.
    Open,
    /// Nobody: the owner creates accounts, `POST /v1/auth/users`.
    Closed,
}

impl SignupPolicy {
    /// `WHEEL_SIGNUP`. Unset or empty is closed: every account's agents run as the daemon's user, so
    /// whether strangers may create one is a decision an operator makes out loud, never one inferred
    /// from how the box looks. Anything unrecognised refuses to boot.
    pub fn parse(raw: Option<&str>) -> Result<Self> {
        match raw.map(str::trim) {
            None | Some("") | Some("closed") => Ok(Self::Closed),
            Some("open") => Ok(Self::Open),
            Some("invite") => bail!(
                "WHEEL_SIGNUP=invite is not supported yet: use \"closed\" and have the owner \
                 create accounts with POST /v1/auth/users"
            ),
            Some(other) => bail!("WHEEL_SIGNUP must be \"open\" or \"closed\", got {other:?}"),
        }
    }
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
    /// Whether `POST /v1/auth/signup` creates accounts. Only meaningful when `auth_mode == Local`.
    pub signup: SignupPolicy,
    /// The deployer's identity system. `Some` exactly when `auth_mode == External`; the mode and
    /// the block cannot disagree, because `from_env` refuses to boot if they do.
    pub external: Option<ExternalAuth>,

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
    /// Live WebSocket bridges one project may hold **on this replica** (ADVERSARY 011). Per
    /// replica, not global: with N replicas the effective ceiling is N times this. It is a
    /// blast-radius bound rather than a quota, and `docs/API.md` says so.
    pub ws_max_bridges_per_project: usize,
    /// Absolute lifetime of a bridge, after which it closes and the client takes a new ws-ticket.
    /// Defence in depth: it bounds how long a missed revocation can persist even if both the
    /// notification and the periodic re-check fail.
    pub ws_max_lifetime_secs: u64,
}

/// How a deployer's identity system proves who a caller is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExternalVerifier {
    /// A JWT from the deployer's issuer, verified against keys they publish.
    Jwks {
        url: String,
        /// The algorithms this deployment will accept, as an explicit operator choice. Never
        /// inferred from a token, and never a permissive default.
        algs: Vec<jsonwebtoken::Algorithm>,
    },
    /// A reverse proxy has already authenticated the user and names them in a header. Wheel
    /// verifies nothing about the assertion, so the trusted-peer check is the whole control.
    ProxyHeader {
        subject_header: String,
        email_header: Option<String>,
    },
}

/// What happens the first time a verified external subject appears.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provision {
    /// Create a Wheel account and link it. Right when the IdP's population is the Wheel population.
    Auto,
    /// Refuse until somebody links the subject to an account. Right when the IdP lets anyone sign
    /// up, where `Auto` would mean anyone on the internet gets a Wheel account.
    Linked,
}

/// The deployer's identity system, as configuration.
///
/// Everything the verifier does is named here, and nothing has a permissive default: an unset
/// audience, algorithm list or provisioning policy refuses to boot rather than guessing. Accepting
/// an unintended algorithm or audience is a cross-tenant compromise, so "it booted" must not be
/// achievable by leaving a field out.
#[derive(Debug, Clone)]
pub struct ExternalAuth {
    /// The operator's label for this provider. Display and logs only — the identity key is the
    /// issuer, because a label can be retitled and an `iss` is what was actually asserted.
    pub provider: String,
    pub issuer: String,
    /// Exact strings. Compared by equality, never by prefix: `wheel:` as a prefix would accept
    /// `wheel:some-other-deployment`, which is the confusion this exists to stop.
    pub audiences: Vec<String>,
    /// Require our audience to be the *only* one. Off by default — multi-audience access tokens
    /// are ordinary — and on when the deployer does not trust the parties named alongside them.
    pub sole_audience: bool,
    /// Which claim is the subject. `sub` unless the IdP has a better immutable id (`oid` on Entra).
    pub subject_claim: String,
    pub azp: Vec<String>,
    /// When set, the token must carry `iat` and live no longer than this.
    pub max_ttl_secs: Option<i64>,
    /// A non-standard header to read the credential from, e.g. `cf-access-jwt-assertion`.
    pub token_header: Option<String>,
    pub provision: Provision,
    pub verifier: ExternalVerifier,
}

/// Every variable this block reads. Listed once so "set but not read" can be detected.
const EXTERNAL_VARS: &[&str] = &[
    "WHEEL_EXTERNAL_VERIFIER",
    "WHEEL_EXTERNAL_PROVIDER",
    "WHEEL_EXTERNAL_ISSUER",
    "WHEEL_EXTERNAL_AUDIENCE",
    "WHEEL_EXTERNAL_ALGS",
    "WHEEL_EXTERNAL_JWKS_URL",
    "WHEEL_EXTERNAL_TOKEN_HEADER",
    "WHEEL_EXTERNAL_SUBJECT_CLAIM",
    "WHEEL_EXTERNAL_AZP",
    "WHEEL_EXTERNAL_MAX_TTL_SECS",
    "WHEEL_EXTERNAL_SOLE_AUDIENCE",
    "WHEEL_EXTERNAL_PROVISION",
    "WHEEL_EXTERNAL_PROXY_SUBJECT_HEADER",
    "WHEEL_EXTERNAL_PROXY_EMAIL_HEADER",
];

impl ExternalAuth {
    /// `Some` exactly under `AUTH_MODE=external`.
    ///
    /// A `WHEEL_EXTERNAL_*` variable set under any other mode refuses to boot. A knob that looks
    /// configured and is never read is how a deployer comes to believe they have pinned an
    /// audience; there is no safe way to ignore one.
    pub fn from_env(mode: AuthMode, env: Env) -> Result<Option<Self>> {
        let present: Vec<&str> = EXTERNAL_VARS
            .iter()
            .copied()
            .filter(|k| std::env::var(k).is_ok_and(|v| !v.trim().is_empty()))
            .collect();

        if mode != AuthMode::External {
            if !present.is_empty() {
                bail!(
                    "{} is set but AUTH_MODE is not \"external\", so it would never be read. \
                     Either set AUTH_MODE=external or unset it.",
                    present.join(", ")
                );
            }
            return Ok(None);
        }

        let provider = var_or("WHEEL_EXTERNAL_PROVIDER", "external");
        let issuer = required("WHEEL_EXTERNAL_ISSUER")?;

        let audiences: Vec<String> = var_or("WHEEL_EXTERNAL_AUDIENCE", "")
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        if audiences.is_empty() {
            bail!(
                "WHEEL_EXTERNAL_AUDIENCE must name at least one audience. An unvalidated audience \
                 means a token minted for a different relying party — another Wheel deployment, a \
                 relay, anything else your issuer serves — is accepted here as its subject."
            );
        }

        let verifier = match var_or("WHEEL_EXTERNAL_VERIFIER", "").trim() {
            "jwks" => ExternalVerifier::Jwks {
                url: required("WHEEL_EXTERNAL_JWKS_URL")?,
                algs: parse_algs(&required("WHEEL_EXTERNAL_ALGS")?)?,
            },
            "proxy_header" => {
                let trusted = crate::http::client_ip::TrustedProxies::from_env()
                    .map_err(|e| anyhow!("{e}"))?;
                if trusted.is_empty() {
                    bail!(
                        "WHEEL_EXTERNAL_VERIFIER=proxy_header requires WHEEL_TRUSTED_PROXIES. \
                         This mode believes a header, so anything that can reach Wheel directly \
                         can be anyone; with no trusted peer list that is everyone."
                    );
                }
                ExternalVerifier::ProxyHeader {
                    subject_header: required("WHEEL_EXTERNAL_PROXY_SUBJECT_HEADER")?
                        .to_ascii_lowercase(),
                    email_header: std::env::var("WHEEL_EXTERNAL_PROXY_EMAIL_HEADER")
                        .ok()
                        .map(|s| s.trim().to_ascii_lowercase())
                        .filter(|s| !s.is_empty()),
                }
            }
            "" => bail!("WHEEL_EXTERNAL_VERIFIER must be set (\"jwks\" or \"proxy_header\")"),
            other => bail!(
                "WHEEL_EXTERNAL_VERIFIER must be \"jwks\" or \"proxy_header\", got {other:?}"
            ),
        };

        let provision = match var_or("WHEEL_EXTERNAL_PROVISION", "").trim() {
            "auto" => Provision::Auto,
            "linked" => Provision::Linked,
            "" => bail!(
                "WHEEL_EXTERNAL_PROVISION must be set: \"auto\" creates a Wheel account for any \
                 subject your issuer vouches for, \"linked\" refuses until an operator links one. \
                 If your issuer lets anyone sign up, \"auto\" lets anyone into Wheel — so this is \
                 a decision to make out loud, never one to default."
            ),
            other => bail!("WHEEL_EXTERNAL_PROVISION must be \"auto\" or \"linked\", got {other:?}"),
        };

        let max_ttl_secs = match std::env::var("WHEEL_EXTERNAL_MAX_TTL_SECS") {
            Ok(v) if !v.trim().is_empty() => {
                let n: i64 = v
                    .trim()
                    .parse()
                    .map_err(|_| anyhow!("WHEEL_EXTERNAL_MAX_TTL_SECS must be a whole number"))?;
                if n <= 0 {
                    bail!("WHEEL_EXTERNAL_MAX_TTL_SECS must be positive");
                }
                Some(n)
            }
            _ => None,
        };

        let ext = ExternalAuth {
            provider,
            issuer,
            audiences,
            sole_audience: matches!(
                var_or("WHEEL_EXTERNAL_SOLE_AUDIENCE", "").trim(),
                "1" | "true" | "yes"
            ),
            subject_claim: {
                let c = var_or("WHEEL_EXTERNAL_SUBJECT_CLAIM", "sub").trim().to_string();
                if c.is_empty() {
                    bail!("WHEEL_EXTERNAL_SUBJECT_CLAIM must not be empty");
                }
                c
            },
            azp: var_or("WHEEL_EXTERNAL_AZP", "")
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect(),
            max_ttl_secs,
            token_header: std::env::var("WHEEL_EXTERNAL_TOKEN_HEADER")
                .ok()
                .map(|s| s.trim().to_ascii_lowercase())
                .filter(|s| !s.is_empty()),
            provision,
            verifier,
        };

        if env == Env::Prod {
            if let ExternalVerifier::Jwks { url, .. } = &ext.verifier {
                reject_local_identity_provider("WHEEL_EXTERNAL_JWKS_URL", url)?;
                reject_local_identity_provider("WHEEL_EXTERNAL_ISSUER", &ext.issuer)?;
            }
        }
        Ok(Some(ext))
    }

    /// Checks that need the rest of the configuration to be built first.
    pub(crate) fn cross_check(&self, cfg: &Config) -> Result<()> {
        // Two issuers that compare equal are two token populations that can stand in for each
        // other. Our own session issuer above all: a local session JWT must never route to the
        // external verifier, nor the reverse.
        if !cfg.clerk_issuer.trim().is_empty() && self.issuer == cfg.clerk_issuer.trim() {
            bail!("WHEEL_EXTERNAL_ISSUER must not equal CLERK_ISSUER: two verifiers pinned to one issuer can stand in for each other");
        }
        if self.issuer == cfg.public_base_url.trim_end_matches('/') {
            bail!("WHEEL_EXTERNAL_ISSUER must not equal PUBLIC_BASE_URL: that is the issuer of this API's own sessions");
        }
        // Ambient credentials plus a browser is CSRF. `routes` refuses a cross-origin request in
        // this mode, and an origin allowlist here would be a second, contradictory answer.
        if matches!(self.verifier, ExternalVerifier::ProxyHeader { .. }) {
            tracing::warn!(
                provider = %self.provider,
                "AUTH_MODE=external with proxy-header auth: identity is whatever a trusted proxy \
                 asserts. Wheel MUST NOT be reachable except through that proxy."
            );
        }
        Ok(())
    }

    #[cfg(test)]
    pub fn for_test() -> Self {
        ExternalAuth {
            provider: "test".into(),
            issuer: "https://issuer.example".into(),
            audiences: vec!["wheel-test".into()],
            sole_audience: false,
            subject_claim: "sub".into(),
            azp: Vec::new(),
            max_ttl_secs: None,
            token_header: None,
            provision: Provision::Auto,
            verifier: ExternalVerifier::Jwks {
                url: "https://issuer.example/jwks".into(),
                algs: vec![jsonwebtoken::Algorithm::RS256, jsonwebtoken::Algorithm::EdDSA],
            },
        }
    }

    #[cfg(test)]
    pub fn for_test_proxy() -> Self {
        ExternalAuth {
            verifier: ExternalVerifier::ProxyHeader {
                subject_header: "x-forwarded-user".into(),
                email_header: Some("x-forwarded-email".into()),
            },
            ..Self::for_test()
        }
    }
}

/// Parse the operator's algorithm allowlist.
///
/// Only the two asymmetric algorithms the key loader can produce are accepted. A symmetric one is
/// refused by name rather than ignored: `HS256` in a list beside a public key set is the classic
/// confusion attack spelled out in configuration, and silently dropping it would leave an operator
/// believing they had enabled something. Anything else — `RS512`, `ES256` — is refused because
/// `auth::jwks` holds no key that could match it, so accepting the word would produce a deployment
/// that boots and then rejects every token for a reason nobody can see.
fn parse_algs(raw: &str) -> Result<Vec<jsonwebtoken::Algorithm>> {
    use jsonwebtoken::Algorithm;
    let mut out = Vec::new();
    for name in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let alg = match name.to_ascii_uppercase().as_str() {
            "RS256" => Algorithm::RS256,
            "EDDSA" | "ED25519" => Algorithm::EdDSA,
            "HS256" | "HS384" | "HS512" => bail!(
                "WHEEL_EXTERNAL_ALGS names {name}, a symmetric algorithm. The key that verifies is \
                 then also the key that signs, so anyone who can verify can forge. Refused."
            ),
            other => bail!(
                "WHEEL_EXTERNAL_ALGS names {other}, which this build cannot verify. Supported: \
                 RS256, EdDSA."
            ),
        };
        if !out.contains(&alg) {
            out.push(alg);
        }
    }
    if out.is_empty() {
        bail!("WHEEL_EXTERNAL_ALGS must name at least one algorithm (RS256, EdDSA)");
    }
    Ok(out)
}

fn required(key: &str) -> Result<String> {
    let v = std::env::var(key).unwrap_or_default().trim().to_string();
    if v.is_empty() {
        bail!("{key} must be set and non-empty under AUTH_MODE=external");
    }
    Ok(v)
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
            Some("external") => AuthMode::External,
            Some(other) => {
                bail!("AUTH_MODE must be \"local\", \"jwks\" or \"external\", got {other:?}")
            }
            None if env == Env::Dev => AuthMode::Local,
            None => {
                bail!("AUTH_MODE must be set in production (\"local\", \"jwks\" or \"external\")")
            }
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
            AuthMode::Jwks | AuthMode::External => Secret::new(String::new()),
        };

        let clerk_azp = var_or("CLERK_AZP", "")
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        let external = ExternalAuth::from_env(auth_mode, env)?;

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
            signup: SignupPolicy::parse(std::env::var("WHEEL_SIGNUP").ok().as_deref())?,
            external,
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
            ws_max_bridges_per_project: parse_or("WS_MAX_BRIDGES_PER_PROJECT", 16usize)?,
            ws_max_lifetime_secs: parse_or("WS_MAX_LIFETIME_SECS", 3600u64)?,
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
        if let Some(ext) = &cfg.external {
            ext.cross_check(&cfg)?;
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
            .field("signup", &self.signup)
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

#[cfg(test)]
mod tests {
    use super::SignupPolicy;

    #[test]
    fn signup_is_closed_unless_opened_and_an_unknown_value_refuses_to_boot() {
        assert_eq!(SignupPolicy::parse(None).unwrap(), SignupPolicy::Closed);
        assert_eq!(SignupPolicy::parse(Some("")).unwrap(), SignupPolicy::Closed);
        assert_eq!(
            SignupPolicy::parse(Some("closed")).unwrap(),
            SignupPolicy::Closed
        );
        assert_eq!(
            SignupPolicy::parse(Some(" open ")).unwrap(),
            SignupPolicy::Open
        );
        let invite = SignupPolicy::parse(Some("invite")).unwrap_err().to_string();
        assert!(invite.contains("POST /v1/auth/users"), "{invite}");
        let other = SignupPolicy::parse(Some("sometimes"))
            .unwrap_err()
            .to_string();
        assert!(other.contains("WHEEL_SIGNUP"), "{other}");
    }
}
