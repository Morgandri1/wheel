//! Host configuration.

use anyhow::{bail, Context, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    /// Local dev / any VM with a docker daemon: one container per project.
    Docker,
    /// Production on Railway, where no docker daemon exists: one unix uid per project.
    Process,
    /// Dev only: an engine already running at `ENGINE_BASE_URL`. Provides no isolation, so it is
    /// rejected unless `WHEEL_ENV=dev`.
    External,
}

#[derive(Clone)]
pub struct Config {
    pub bind_addr: String,
    pub secret: String,
    pub backend: Backend,
    pub data_dir: String,
    pub engine_image: String,
    pub docker_network: String,
    pub engine_port: u16,
    pub memory_bytes: i64,
    pub nano_cpus: i64,
    pub pids_limit: i64,
    pub start_timeout_secs: u64,
    /// First uid handed to a project in the process backend.
    pub uid_range_start: u32,
    /// How many consecutive uids each project owns: the engine at `base`, its nodes above it.
    pub uid_stride: u32,
    /// Where per-project engine sockets live. One 0700 directory per project underneath.
    pub run_dir: String,
    /// Only meaningful for the external backend.
    pub engine_base_url: String,
}

fn var(k: &str) -> Result<String> {
    std::env::var(k).with_context(|| format!("required environment variable {k} is not set"))
}
fn var_or(k: &str, d: &str) -> String {
    std::env::var(k).unwrap_or_else(|_| d.to_string())
}
fn parse_or<T: std::str::FromStr>(k: &str, d: T) -> Result<T> {
    match std::env::var(k) {
        Ok(v) => v
            .parse()
            .map_err(|_| anyhow::anyhow!("{k} is not a valid {}", std::any::type_name::<T>())),
        Err(_) => Ok(d),
    }
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let secret = var("WHEEL_HOST_SECRET")?;
        // This secret is the *only* thing standing between anything that can reach this port and
        // full control of every tenant's sandbox. A short or absent one is not a warning.
        // The host must never be reachable from the internet (§5b). Railway sets
        // RAILWAY_PUBLIC_DOMAIN only when a public domain exists, so its presence means someone
        // has exposed the sandbox supervisor — every tenant's engine, behind one bearer.
        //
        // This is not hypothetical: a bare `railway domain` with this service linked created one
        // by accident, and nothing in the system would have noticed. Refusing to boot turns a
        // silent exposure into an obvious outage, which is the trade you want for this process.
        // ALLOW_PUBLIC_DOMAIN exists only so a deliberate future topology is not blocked by me.
        if let Ok(domain) = std::env::var("RAILWAY_PUBLIC_DOMAIN") {
            let domain = domain.trim();
            if !domain.is_empty() && var_or("ALLOW_PUBLIC_DOMAIN", "0") != "1" {
                bail!(
                    "refusing to start: a public domain ({domain}) is attached to this host. \
                     The sandbox supervisor must be reachable on private networking only — it \
                     fronts every tenant's engine behind a single bearer. Remove the domain, or \
                     set ALLOW_PUBLIC_DOMAIN=1 if this is deliberate."
                );
            }
        }

        if secret.len() < 16 {
            bail!("WHEEL_HOST_SECRET must be at least 16 characters");
        }

        let backend = match var_or("SANDBOX_BACKEND", "docker").as_str() {
            "docker" => Backend::Docker,
            "process" => Backend::Process,
            "external" => {
                // This backend isolates nothing; it forwards to a URL. Allowing it outside dev
                // would mean shipping a "sandbox" that is not a sandbox.
                if var_or("WHEEL_ENV", "prod") != "dev" {
                    bail!(
                        "SANDBOX_BACKEND=external requires WHEEL_ENV=dev; it provides no isolation"
                    );
                }
                Backend::External
            }
            other => bail!(
                "SANDBOX_BACKEND must be \"docker\", \"process\" or \"external\", got {other:?}"
            ),
        };

        Ok(Config {
            bind_addr: var_or("BIND_ADDR", "0.0.0.0:7100"),
            secret,
            backend,
            data_dir: var_or("WHEEL_DATA_DIR", "/data"),
            engine_image: var_or("ENGINE_IMAGE", "wheel-engine:dev"),
            docker_network: var_or("DOCKER_NETWORK", "wheel"),
            engine_port: parse_or("ENGINE_PORT", 7000u16)?,
            memory_bytes: parse_or("CONTAINER_MEMORY_MB", 1024i64)? * 1024 * 1024,
            nano_cpus: (parse_or("CONTAINER_CPUS", 1.0f64)? * 1e9) as i64,
            pids_limit: parse_or("CONTAINER_PIDS_LIMIT", 512i64)?,
            start_timeout_secs: parse_or("START_TIMEOUT_SECS", 30u64)?,
            uid_range_start: parse_or("UID_RANGE_START", 20_000u32)?,
            uid_stride: parse_or("UID_STRIDE", 64u32)?,
            run_dir: var_or("WHEEL_RUN_DIR", "/run/wheel"),
            engine_base_url: var_or("ENGINE_BASE_URL", "http://127.0.0.1:7000"),
        })
    }

    pub fn container_name(&self, id: &uuid::Uuid) -> String {
        format!("wheel-p-{id}")
    }
    pub fn volume_name(&self, id: &uuid::Uuid) -> String {
        format!("wheel-p-{id}-data")
    }
    pub fn engine_url(&self, id: &uuid::Uuid) -> String {
        format!("http://wheel-p-{}:{}", id, self.engine_port)
    }
}
