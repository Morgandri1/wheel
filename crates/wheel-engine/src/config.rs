//! Engine configuration, read once at boot from the §4b spawn contract.
//!
//! Misconfiguration must fail loudly and immediately — the host treats a
//! non-zero exit with a one-line reason as "this sandbox is broken", which is
//! far better than a half-configured engine that accepts traffic.

use std::path::PathBuf;

use wheel_core::{spawn::*, ListenAddr};

#[derive(Debug, Clone)]
pub struct Config {
    pub project_id: uuid::Uuid,
    pub engine_secret: String,
    pub vault_key: Option<String>,
    pub data_dir: PathBuf,
    pub listen: ListenAddr,
    pub json_logs: bool,
    /// Exact `host:port` targets a tool call may reach despite the SSRF policy.
    ///
    /// For testing and red-team probes ONLY: the engine refuses to boot with
    /// this set in production. See [`ENV_TOOL_ALLOW_HOST`].
    pub tool_allow_hosts: Vec<String>,
}

/// Exact `host:port` targets a tool call may reach despite the SSRF policy.
///
/// Comma-separated, exact matches only — no wildcards, no CIDR, no bare hosts.
/// Consulted AFTER the address is resolved and pinned, so it permits one
/// literal target rather than opening a range.
pub const ENV_TOOL_ALLOW_HOST: &str = "WHEEL_TOOL_ALLOW_HOST";

/// `prod` here makes the allowlist a boot failure rather than a warning.
pub const ENV_ENV: &str = "WHEEL_ENV";

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("{0} is required")]
    Missing(&'static str),
    #[error("{0} must be a uuid")]
    BadUuid(&'static str),
    #[error("{0}")]
    BadListen(#[from] wheel_core::ListenAddrError),
    #[error("{ENV_ENGINE_SECRET} must be at least 16 characters")]
    WeakSecret,
    /// The allowlist exists to let tests and red-team probes reach a local
    /// target. In production it is a hole in the SSRF policy, so it is a
    /// refusal to start rather than a warning nobody reads.
    #[error(
        "{ENV_TOOL_ALLOW_HOST} is set ({0}) but {ENV_ENV}=prod: that allowlist bypasses the \
         SSRF policy and must never be set in production"
    )]
    AllowlistInProd(String),
    #[error("{ENV_TOOL_ALLOW_HOST} entry {0:?} must be an exact host:port")]
    BadAllowEntry(String),
}

impl Config {
    pub fn from_env() -> Result<Self, ConfigError> {
        fn var(k: &'static str) -> Result<String, ConfigError> {
            std::env::var(k)
                .ok()
                .filter(|v| !v.trim().is_empty())
                .ok_or(ConfigError::Missing(k))
        }

        let project_id = var(ENV_PROJECT_ID)?
            .parse()
            .map_err(|_| ConfigError::BadUuid(ENV_PROJECT_ID))?;

        let engine_secret = var(ENV_ENGINE_SECRET)?;
        // A short secret is a configuration bug, not a policy preference: this
        // bearer is the entire control-plane boundary.
        if engine_secret.len() < 16 {
            return Err(ConfigError::WeakSecret);
        }

        let listen = match std::env::var(ENV_LISTEN) {
            Ok(v) if !v.trim().is_empty() => ListenAddr::parse(&v)?,
            _ => ListenAddr::default_tcp(),
        };

        Ok(Self {
            project_id,
            engine_secret,
            vault_key: std::env::var(ENV_VAULT_KEY).ok().filter(|v| !v.is_empty()),
            data_dir: PathBuf::from(var(ENV_DATA_DIR).unwrap_or_else(|_| "/data".into())),
            listen,
            json_logs: std::env::var(ENV_LOG).map(|v| v == "json").unwrap_or(false),
            tool_allow_hosts: tool_allow_hosts()?,
        })
    }

    pub fn db_path(&self) -> PathBuf {
        self.data_dir.join("wheel.db")
    }
    pub fn chest_dir(&self) -> PathBuf {
        self.data_dir.join("chest")
    }
    pub fn scripts_dir(&self) -> PathBuf {
        self.data_dir.join("scripts")
    }
    pub fn creds_dir(&self) -> PathBuf {
        self.data_dir.join("creds")
    }
    /// Per-node runtime dir holding the 0600 token file and the prompt file —
    /// neither may ever go on a command line or into the environment.
    pub fn node_run_dir(&self, node: uuid::Uuid) -> PathBuf {
        self.data_dir.join("run").join(node.to_string())
    }
}

/// Parse and police the tool allowlist.
fn tool_allow_hosts() -> Result<Vec<String>, ConfigError> {
    let raw = std::env::var(ENV_TOOL_ALLOW_HOST).unwrap_or_default();
    let entries: Vec<String> = raw
        .split(',')
        .map(str::trim)
        .filter(|e| !e.is_empty())
        .map(str::to_string)
        .collect();
    if entries.is_empty() {
        return Ok(entries);
    }

    let is_prod = std::env::var(ENV_ENV).map(|v| v == "prod").unwrap_or(false);
    if is_prod {
        return Err(ConfigError::AllowlistInProd(entries.join(",")));
    }

    // Exact host:port only. A bare host would permit every port on it, and a
    // wildcard would be a range wearing an allowlist's clothes.
    for e in &entries {
        let ok = e
            .rsplit_once(':')
            .is_some_and(|(h, p)| !h.is_empty() && p.parse::<u16>().is_ok() && !h.contains('*'));
        if !ok {
            return Err(ConfigError::BadAllowEntry(e.clone()));
        }
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Env is process-global, so these run one at a time.
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn with_env(vars: &[(&str, Option<&str>)], f: impl FnOnce()) {
        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved: Vec<_> = vars
            .iter()
            .map(|(k, _)| (*k, std::env::var(k).ok()))
            .collect();
        for (k, v) in vars {
            match v {
                Some(v) => std::env::set_var(k, v),
                None => std::env::remove_var(k),
            }
        }
        f();
        for (k, v) in saved {
            match v {
                Some(v) => std::env::set_var(k, v),
                None => std::env::remove_var(k),
            }
        }
    }

    /// The allowlist bypasses the SSRF policy. In production that is a hole,
    /// so it is a refusal to START rather than a warning nobody reads — the
    /// engine failing to boot is loud, and the host reports it.
    #[test]
    fn the_allowlist_is_a_boot_failure_in_production() {
        with_env(
            &[
                (ENV_TOOL_ALLOW_HOST, Some("127.0.0.1:8080")),
                (ENV_ENV, Some("prod")),
            ],
            || {
                let err = tool_allow_hosts().unwrap_err();
                let msg = err.to_string();
                assert!(matches!(err, ConfigError::AllowlistInProd(_)), "{msg}");
                assert!(msg.contains("127.0.0.1:8080"), "name the targets: {msg}");
                assert!(msg.contains("production"), "{msg}");
            },
        );
    }

    #[test]
    fn outside_production_the_allowlist_is_accepted_as_written() {
        with_env(
            &[
                (ENV_TOOL_ALLOW_HOST, Some("127.0.0.1:8080, 127.0.0.1:9090")),
                (ENV_ENV, Some("dev")),
            ],
            || {
                assert_eq!(
                    tool_allow_hosts().unwrap(),
                    vec!["127.0.0.1:8080".to_string(), "127.0.0.1:9090".to_string()]
                );
            },
        );
        // ...and unset is the normal case: empty, permitting nothing.
        with_env(&[(ENV_TOOL_ALLOW_HOST, None), (ENV_ENV, None)], || {
            assert!(tool_allow_hosts().unwrap().is_empty());
        });
    }

    /// Exact host:port only. A bare host would permit every port on it, and a
    /// wildcard would be a range wearing an allowlist's clothes.
    #[test]
    fn only_an_exact_host_and_port_is_a_valid_entry() {
        for bad in [
            "127.0.0.1",
            "*:8080",
            "127.0.0.*:8080",
            "127.0.0.1:",
            ":8080",
            "127.0.0.1:http",
        ] {
            with_env(
                &[(ENV_TOOL_ALLOW_HOST, Some(bad)), (ENV_ENV, Some("dev"))],
                || {
                    assert!(
                        matches!(tool_allow_hosts(), Err(ConfigError::BadAllowEntry(_))),
                        "{bad:?} should not be a valid entry"
                    );
                },
            );
        }
    }

    /// An empty or whitespace-only value is "not set", not an error — an
    /// operator clearing the variable should not have to delete it.
    #[test]
    fn a_blank_value_reads_as_unset() {
        for blank in ["", "   ", ",", " , "] {
            with_env(
                &[(ENV_TOOL_ALLOW_HOST, Some(blank)), (ENV_ENV, Some("prod"))],
                || {
                    assert!(
                        tool_allow_hosts().unwrap().is_empty(),
                        "{blank:?} should read as unset, even in prod"
                    );
                },
            );
        }
    }
}
