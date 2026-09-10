// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

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
    // Per-child rlimits. Defaults are sized so a cargo/pnpm build inside a sandbox completes;
    // see Rlimits in sandbox/process.rs for why AS and CPU default to unlimited.
    pub rlimit_nproc: u64,
    pub rlimit_address_space_bytes: Option<u64>,
    pub rlimit_fsize_bytes: u64,
    pub rlimit_nofile: u64,
    pub rlimit_cpu_secs: Option<u64>,
    /// How long a process left over from a previous engine gets to exit on SIGTERM before it is
    /// killed. Short on purpose: this runs on the start path, once per project, on host boot.
    pub reap_grace_secs: u64,
    /// Megabytes that must be free before a project may start. Not what a build needs — what one
    /// engine needs to open its database and take a message without corrupting it.
    pub disk_floor_mb: u64,
    /// How many projects the boot reconcile brings back at once.
    pub reconcile_concurrency: usize,
    /// Only meaningful for the external backend.
    pub engine_base_url: String,
    /// Project ids whose engines get `WHEEL_HARNESS_AUTH=oauth-token` instead of the fail-secure
    /// `api-key-only` every other project gets (`docs/proposals/wheeld-first-class-cloud-api-key-
    /// policy.md`, wow-agent-brief task 4). Deliberately host config, never a project-reachable
    /// value — see [`Config::harness_auth_for`].
    pub oauth_allowed_projects: Vec<uuid::Uuid>,
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

/// `WHEEL_HARNESS_AUTH_OAUTH_PROJECTS`: comma-separated project ids, empty/unset meaning "none" —
/// never "not configured, so allow everything." A malformed entry fails the boot naming the exact
/// token, the same discipline `crates/wheel-engine/src/config.rs`'s `harness_auth()` already uses
/// for the env var this one gates: a typo that silently dropped an id would look identical to "the
/// operator successfully exempted this project" right up until it doesn't.
fn parse_oauth_allowlist() -> Result<Vec<uuid::Uuid>> {
    let raw = var_or("WHEEL_HARNESS_AUTH_OAUTH_PROJECTS", "");
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| {
            s.parse::<uuid::Uuid>().map_err(|_| {
                anyhow::anyhow!(
                    "WHEEL_HARNESS_AUTH_OAUTH_PROJECTS contains {s:?}, which is not a valid \
                     project id (uuid)"
                )
            })
        })
        .collect()
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
            // BIND_ADDR wins if set; otherwise $PORT, which is what the platform's health
            // checker probes. Binding 7100 while the platform checks $PORT means every probe
            // reaches nothing, the replica is declared unhealthy and the container is stopped —
            // which has taken this service down twice, once per health-check path tried.
            bind_addr: match (std::env::var("BIND_ADDR"), std::env::var("PORT")) {
                (Ok(a), _) if !a.trim().is_empty() => a,
                (_, Ok(p)) if !p.trim().is_empty() => format!("0.0.0.0:{}", p.trim()),
                _ => "0.0.0.0:7100".to_string(),
            },
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
            rlimit_nproc: parse_or("RLIMIT_NPROC", 4096u64)?,
            // 0 means unlimited, which is the default: a virtual-address-space cap is what kills
            // rustc, and the machine's cgroup is what should bound real memory.
            rlimit_address_space_bytes: match parse_or("RLIMIT_AS_BYTES", 0u64)? {
                0 => None,
                n => Some(n),
            },
            rlimit_fsize_bytes: parse_or("RLIMIT_FSIZE_BYTES", 8 * 1024 * 1024 * 1024u64)?,
            rlimit_nofile: parse_or("RLIMIT_NOFILE", 16384u64)?,
            rlimit_cpu_secs: match parse_or("RLIMIT_CPU_SECS", 0u64)? {
                0 => None,
                n => Some(n),
            },
            reap_grace_secs: parse_or("REAP_GRACE_SECS", 5u64)?,
            disk_floor_mb: parse_or("DISK_FLOOR_MB", 256u64)?,
            reconcile_concurrency: parse_or("RECONCILE_CONCURRENCY", 8usize)?,
            engine_base_url: var_or("ENGINE_BASE_URL", "http://127.0.0.1:7000"),
            oauth_allowed_projects: parse_oauth_allowlist()?,
        })
    }

    /// A config with production defaults and no environment, for tests that need a `Config` but
    /// have nothing to say about it.
    #[cfg(test)]
    pub fn for_tests(data_dir: &str) -> Self {
        Config {
            bind_addr: "127.0.0.1:0".into(),
            secret: "test-host-secret".into(),
            backend: Backend::Process,
            data_dir: data_dir.into(),
            engine_image: "wheel-engine:test".into(),
            docker_network: "wheel".into(),
            engine_port: 7000,
            memory_bytes: 1024 * 1024 * 1024,
            nano_cpus: 1_000_000_000,
            pids_limit: 512,
            start_timeout_secs: 30,
            uid_range_start: 20_000,
            uid_stride: 64,
            run_dir: format!("{data_dir}/run"),
            rlimit_nproc: 4096,
            rlimit_address_space_bytes: None,
            rlimit_fsize_bytes: 8 * 1024 * 1024 * 1024,
            rlimit_nofile: 16384,
            rlimit_cpu_secs: None,
            reap_grace_secs: 5,
            disk_floor_mb: 1,
            reconcile_concurrency: 8,
            engine_base_url: "http://127.0.0.1:7000".into(),
            oauth_allowed_projects: Vec::new(),
        }
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

    /// The `WHEEL_HARNESS_AUTH` value this project's engine should be spawned with
    /// (`docs/proposals/wheeld-first-class-cloud-api-key-policy.md`). Fail-secure: a project not on
    /// the allowlist — including an unconfigured allowlist — gets `api-key-only`. `wheel-host` is
    /// the cloud side of this policy; `wheeld` (self-hosted) never calls this at all and keeps the
    /// engine's own permissive default.
    ///
    /// ADVERSARY (task-4 review): whether a redeployed allowlist takes effect immediately depends
    /// on the backend, not on this method — it is pure and re-evaluated on every call. `process`
    /// (production) calls it fresh from `engine_env()` on every single spawn, so a start right
    /// after a `wheel-host` redeploy already sees the new list. `docker` (local dev) bakes env into
    /// the container at `create()` and `start`/`restart` reuse that existing container without
    /// recreating it (`provisioning_an_existing_container_does_not_recreate_it`) — an already-
    /// running docker-backed project keeps its value until the container is destroyed and
    /// reprovisioned. Same class of staleness `WHEEL_PROJECT_ID` and everything else in that env
    /// already has; not new here, and not a path production runs (production is `process`).
    pub fn harness_auth_for(&self, id: &uuid::Uuid) -> &'static str {
        if self.oauth_allowed_projects.contains(id) {
            "oauth-token"
        } else {
            "api-key-only"
        }
    }
}

// Env-var parsing (`WHEEL_HARNESS_AUTH_OAUTH_PROJECTS`, including the boot-failure case) is
// covered in `tests/config_env.rs`'s single sequenced test, alongside every other `Config::from_env`
// case — env vars are process-global, so this crate keeps one test function for them rather than
// several that would race each other. `harness_auth_for` itself takes an already-parsed `Vec`, so
// its tests need no env at all and live here as ordinary unit tests.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn harness_auth_for_is_oauth_token_only_for_a_listed_id() {
        let allowed = uuid::Uuid::new_v4();
        let other = uuid::Uuid::new_v4();
        let mut cfg = Config::for_tests("/tmp/irrelevant");
        cfg.oauth_allowed_projects = vec![allowed];
        assert_eq!(cfg.harness_auth_for(&allowed), "oauth-token");
        assert_eq!(cfg.harness_auth_for(&other), "api-key-only");
    }

    #[test]
    fn an_empty_allowlist_is_api_key_only_for_everyone_fail_secure() {
        let cfg = Config::for_tests("/tmp/irrelevant");
        assert!(cfg.oauth_allowed_projects.is_empty());
        assert_eq!(cfg.harness_auth_for(&uuid::Uuid::new_v4()), "api-key-only");
    }
}
