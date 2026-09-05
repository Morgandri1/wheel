//! The engine **spawn contract** (ARCHITECTURE.md §4b).
//!
//! `wheel-host` (owned by API) starts one `wheel-engine` process per project.
//! This module pins the interface between them — env var names and the listen
//! address grammar — so the two crates cannot drift. The engine reads these;
//! the host writes them.

use std::path::PathBuf;

/// Project this engine serves (uuid).
pub const ENV_PROJECT_ID: &str = "WHEEL_PROJECT_ID";
/// Bearer token the host must present to the engine control plane.
pub const ENV_ENGINE_SECRET: &str = "WHEEL_ENGINE_SECRET";
/// Base64 per-project key used to encrypt vault values at rest.
pub const ENV_VAULT_KEY: &str = "WHEEL_VAULT_KEY";
/// Data directory: sqlite db, chest blobs, scripts, per-node credentials.
pub const ENV_DATA_DIR: &str = "WHEEL_DATA_DIR";
/// Where the control plane listens. See [`ListenAddr`].
pub const ENV_LISTEN: &str = "WHEEL_LISTEN";
/// `json` for structured logs (production), anything else for human logs.
pub const ENV_LOG: &str = "WHEEL_LOG";

/// Which binary an image invocation should be: `engine` (default) or `host`.
/// The single `Dockerfile.host` image ships both.
pub const ENV_ROLE: &str = "WHEEL_ROLE";

// --- passed to agent children, not to the engine ---------------------------

/// Per-node capability token given to an agent/script child.
pub const ENV_TOKEN: &str = "WHEEL_TOKEN";
/// Where the `wheel` CLI should reach its engine.
pub const ENV_ENGINE_URL: &str = "WHEEL_ENGINE_URL";
/// The child's own node name, for display.
pub const ENV_NODE: &str = "WHEEL_NODE";

/// The engine must answer `/healthz` within this long of being spawned, or the
/// host declares the sandbox failed.
pub const HEALTHZ_DEADLINE_SECS: u64 = 10;

/// On SIGTERM the engine stops children and flushes sqlite within this long.
pub const SHUTDOWN_DEADLINE_SECS: u64 = 15;

/// Where the engine's control plane listens.
///
/// Two forms, one per sandbox backend:
/// - `tcp://0.0.0.0:7000` — docker backend; reachable on the container network.
/// - `unix:///run/wheel/<id>/engine.sock` — process backend; the socket is
///   owned by the project's uid, so tenants cannot reach each other's engines
///   even though they share a kernel and a network namespace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListenAddr {
    Tcp(String),
    Unix(PathBuf),
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ListenAddrError {
    #[error("{ENV_LISTEN} must start with tcp:// or unix://, got {0:?}")]
    UnknownScheme(String),
    #[error("{ENV_LISTEN} tcp address must be host:port, got {0:?}")]
    BadTcp(String),
    #[error("{ENV_LISTEN} unix path must be absolute, got {0:?}")]
    RelativeSocketPath(String),
}

impl ListenAddr {
    /// Parse the `WHEEL_LISTEN` value. Defaults to the docker-mode TCP address
    /// when unset, so a bare `docker run` of the image just works.
    pub fn parse(raw: &str) -> Result<Self, ListenAddrError> {
        if let Some(rest) = raw.strip_prefix("tcp://") {
            // Must have a port; reject a bare host, which would otherwise bind
            // to a random one and silently be unreachable.
            let ok = rest
                .rsplit_once(':')
                .is_some_and(|(host, port)| !host.is_empty() && port.parse::<u16>().is_ok());
            if !ok {
                return Err(ListenAddrError::BadTcp(rest.to_string()));
            }
            Ok(ListenAddr::Tcp(rest.to_string()))
        } else if let Some(rest) = raw.strip_prefix("unix://") {
            if !rest.starts_with('/') {
                return Err(ListenAddrError::RelativeSocketPath(rest.to_string()));
            }
            Ok(ListenAddr::Unix(PathBuf::from(rest)))
        } else {
            Err(ListenAddrError::UnknownScheme(raw.to_string()))
        }
    }

    /// The default for docker mode.
    pub fn default_tcp() -> Self {
        ListenAddr::Tcp(format!("0.0.0.0:{}", crate::ENGINE_PORT))
    }
}

impl std::fmt::Display for ListenAddr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ListenAddr::Tcp(a) => write!(f, "tcp://{a}"),
            ListenAddr::Unix(p) => write!(f, "unix://{}", p.display()),
        }
    }
}
