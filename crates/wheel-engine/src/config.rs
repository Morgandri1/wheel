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
}

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
