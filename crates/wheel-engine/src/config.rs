// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

//! Engine configuration, read once at boot from the §4b spawn contract.
//!
//! Misconfiguration must fail loudly and immediately — the host treats a
//! non-zero exit with a one-line reason as "this sandbox is broken", which is
//! far better than a half-configured engine that accepts traffic.

use std::path::PathBuf;

use wheel_core::{spawn::*, ListenAddr};

/// Matches QA's `WHEEL_PROGRESS_RESOLVE_SECS` default so the CI gate and the
/// runtime backstop watch the same signal rather than inventing two
/// definitions of "stuck" (PM, contract b1f76bc).
pub const DEFAULT_STARTUP_DEADLINE_SECS: u64 = 60;

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
    /// How long an agent may stay in `starting` WITH WORK QUEUED before the
    /// engine calls it wedged (ADVERSARY 041).
    ///
    /// Config rather than a global env read so a test can set it per engine:
    /// two tests mutating one process-wide variable race each other, which is
    /// exactly how the first version of this failed — green alone, red in the
    /// suite.
    pub startup_deadline_secs: u64,
    /// Which credential KIND this deployment permits (docs/proposals/
    /// wheel-harness-auth.md). Deployment-level, not project- or agent-level
    /// by design: the thing being gated is which kind is allowed to exist at
    /// all here, and a knob a project's own owner could set would not be a
    /// policy. `wheeld`/`wheel-host` are the only things that set this.
    pub harness_auth: HarnessAuthPolicy,
}

/// `WHEEL_HARNESS_AUTH`'s two values (wheel-harness-auth.md's "Design" §
/// "Where the switch lives").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HarnessAuthPolicy {
    /// Today's behaviour: either an OAuth-shaped credential or an API key may
    /// be stored and used, unrestricted. The default so that upgrading the
    /// engine binary never changes an existing board's behaviour.
    #[default]
    OauthToken,
    /// OAuth-shaped credentials are refused on every surface (auth/complete,
    /// vault PUT, and — the gate that actually holds, since an agent can
    /// self-provision one via its own shell — at spawn and on the periodic
    /// re-check while running).
    ApiKeyOnly,
}

/// Selects which credential kind this deployment permits. See
/// [`HarnessAuthPolicy`].
pub const ENV_HARNESS_AUTH: &str = "WHEEL_HARNESS_AUTH";

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
    #[error(
        "{ENV_HARNESS_AUTH}={0:?} is not a value this engine understands (want \"oauth-token\" \
         or \"api-key-only\", or leave it unset for oauth-token's unrestricted default)"
    )]
    BadHarnessAuth(String),
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

        let vault_key = std::env::var(ENV_VAULT_KEY).ok().filter(|v| !v.is_empty());

        // ADVERSARY 036/037: until per-node uids land (§3e, M2/M3), every
        // child of this engine shares ITS uid, so anything left in the
        // engine's own environ sits in /proc/<engine-pid>/environ, readable
        // by any of them for the engine's entire lifetime. These two are the
        // whole story: the control-plane bearer (bypasses the wire matrix
        // outright) and the key that decrypts every vault in the project. A
        // stopgap independent of the uid work -- scrub the moment they are
        // read, not "when M2 lands".
        //
        // SAFETY: single-threaded here -- this runs once, synchronously, at
        // the top of `main`, before any child is spawned or any other thread
        // that could be reading the environment concurrently exists.
        unsafe {
            std::env::remove_var(ENV_ENGINE_SECRET);
            std::env::remove_var(ENV_VAULT_KEY);
        }

        Ok(Self {
            project_id,
            engine_secret,
            vault_key,
            data_dir: PathBuf::from(var(ENV_DATA_DIR).unwrap_or_else(|_| "/data".into())),
            listen,
            json_logs: std::env::var(ENV_LOG).map(|v| v == "json").unwrap_or(false),
            tool_allow_hosts: tool_allow_hosts()?,
            startup_deadline_secs: std::env::var("WHEEL_STARTUP_DEADLINE_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(DEFAULT_STARTUP_DEADLINE_SECS),
            harness_auth: harness_auth()?,
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
    /// An agent's working copy, per §3e: `/data/projects/<id>/ws/<name>`,
    /// which is what `data_dir` already is inside the sandbox.
    ///
    /// The child's cwd used to be `data_dir` itself, whose child is `creds/`,
    /// so every agent ran with its working directory set to the PARENT of
    /// every node's credential store. `ls .` enumerated them, and anything the
    /// agent wrote — a clone, a build artifact, a stray tempfile — landed in
    /// the same tree as the secrets. One did: a `target/` directory next to
    /// the credential dirs filled the production volume.
    ///
    /// This moves where an agent writes. It is NOT the isolation boundary:
    /// nothing here stops an agent reading `/data/creds`, because today every
    /// child runs as the same uid. That is §2's per-node uid work and it is
    /// still a known gap.
    pub fn workspace_dir(&self, node_name: &str) -> PathBuf {
        self.data_dir.join("ws").join(node_name)
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

/// Parse `WHEEL_HARNESS_AUTH`. Blank/unset reads as the permissive default,
/// same treatment as [`tool_allow_hosts`] gives a blank allowlist — an
/// operator clearing the variable should not have to delete it, and an
/// engine with no opinion about this must not refuse to boot over it.
fn harness_auth() -> Result<HarnessAuthPolicy, ConfigError> {
    let raw = std::env::var(ENV_HARNESS_AUTH).unwrap_or_default();
    match raw.trim() {
        "" => Ok(HarnessAuthPolicy::default()),
        "oauth-token" => Ok(HarnessAuthPolicy::OauthToken),
        "api-key-only" => Ok(HarnessAuthPolicy::ApiKeyOnly),
        other => Err(ConfigError::BadHarnessAuth(other.to_string())),
    }
}

#[cfg(test)]
mod tests {

    /// The `unsafe remove_var` in `from_env` is sound for ONE reason: it runs
    /// at the top of `wheel-engine`'s `main`, before the tokio runtime exists
    /// and before any child is spawned, so nothing can be reading the
    /// environment concurrently. `std::env::remove_var` is undefined behaviour
    /// the moment a second thread is live.
    ///
    /// That safety argument is about the CALL SITE, not about this function,
    /// so nothing in the type system protects it. A future caller inside a
    /// running runtime — `wheeld` builds its runtime first and calls the host
    /// and api configs from inside it — would make this UB silently, with
    /// every test still green.
    ///
    /// So the invariant is asserted where it actually lives: the whole
    /// workspace is scanned, and `Config::from_env` may be named only by this
    /// crate's `main.rs` and by tests.
    #[test]
    fn the_engine_config_is_only_read_before_a_runtime_exists() {
        // Outside this crate the engine's config is only reachable as
        // `wheel_engine::Config`; inside it, unqualified. Checking both
        // spellings by location avoids matching `wheel-host`'s and
        // `wheel-api`'s own `Config::from_env`, which are different types with
        // no `remove_var` in them.
        fn scan(dir: &std::path::Path, out: &mut Vec<String>) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for e in entries.flatten() {
                let path = e.path();
                if path.is_dir() {
                    if path.file_name().is_some_and(|n| n == "target") {
                        continue;
                    }
                    scan(&path, out);
                    continue;
                }
                if !path.extension().is_some_and(|x| x == "rs") {
                    continue;
                }
                let Ok(text) = std::fs::read_to_string(&path) else {
                    continue;
                };
                let shown = path.display().to_string();
                let ours = shown.contains("wheel-engine/");
                let hit = text.lines().any(|l| {
                    if ours {
                        l.contains("Config::from_env")
                    } else {
                        l.contains("wheel_engine::Config::from_env")
                    }
                });
                if hit {
                    out.push(shown);
                }
            }
        }

        let crates = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("crates/ is the parent of this crate")
            .to_path_buf();
        let mut callers = Vec::new();
        scan(&crates, &mut callers);

        let unexpected: Vec<_> = callers
            .iter()
            .filter(|p| {
                // The engine's own entry point, which runs it before building
                // a runtime, and this file's tests.
                !p.ends_with("wheel-engine/src/main.rs")
                    && !p.ends_with("wheel-engine/src/config.rs")
            })
            .collect();

        assert!(
            unexpected.is_empty(),
            "Config::from_env calls `unsafe std::env::remove_var`, which is UB unless it runs \
             single-threaded before any runtime. New caller(s) found: {unexpected:?} — if one of \
             these runs inside a tokio runtime, the unsafe block is no longer sound."
        );
    }
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

    /// ADVERSARY 036/037: until per-node uids land, every child of this
    /// engine shares its uid, so anything `from_env` leaves behind sits in
    /// `/proc/<engine-pid>/environ` for any of them to read for the engine's
    /// whole lifetime. The two that matter are the control-plane bearer and
    /// the vault-decryption key — both must be gone from the process
    /// environment the moment `Config` has its own copy, not merely absent
    /// from the returned struct.
    #[test]
    fn the_engine_secret_and_vault_key_do_not_survive_in_the_process_environment() {
        with_env(
            &[
                (ENV_PROJECT_ID, Some("2b1f6b0e-6b0a-4c1a-9c1a-000000000000")),
                (ENV_ENGINE_SECRET, Some("at-least-sixteen-characters")),
                (ENV_VAULT_KEY, Some("some-vault-key")),
                (ENV_LISTEN, None),
                (ENV_DATA_DIR, None),
                (ENV_LOG, None),
                (ENV_TOOL_ALLOW_HOST, None),
                (ENV_ENV, None),
            ],
            || {
                let cfg = Config::from_env().expect("a fully-specified env must configure");

                // The struct still has them -- this is a scrub, not a loss.
                assert_eq!(cfg.engine_secret, "at-least-sixteen-characters");
                assert_eq!(cfg.vault_key.as_deref(), Some("some-vault-key"));

                // The process environment -- what a same-uid child's
                // /proc/<engine-pid>/environ would show -- must not.
                assert!(
                    std::env::var(ENV_ENGINE_SECRET).is_err(),
                    "the engine secret is still in this process's environment"
                );
                assert!(
                    std::env::var(ENV_VAULT_KEY).is_err(),
                    "the vault key is still in this process's environment"
                );
            },
        );
    }

    /// Unset (or blank) is `oauth-token` -- today's unrestricted behaviour --
    /// so upgrading the engine binary never changes an existing board's
    /// behaviour on its own (wheel-harness-auth.md's "Default, unset" clause).
    #[test]
    fn harness_auth_defaults_to_oauth_token_when_unset_or_blank() {
        for blank in [None, Some(""), Some("   ")] {
            with_env(&[(ENV_HARNESS_AUTH, blank)], || {
                assert_eq!(harness_auth().unwrap(), HarnessAuthPolicy::OauthToken);
            });
        }
    }

    #[test]
    fn harness_auth_recognises_both_documented_values() {
        with_env(&[(ENV_HARNESS_AUTH, Some("oauth-token"))], || {
            assert_eq!(harness_auth().unwrap(), HarnessAuthPolicy::OauthToken);
        });
        with_env(&[(ENV_HARNESS_AUTH, Some("api-key-only"))], || {
            assert_eq!(harness_auth().unwrap(), HarnessAuthPolicy::ApiKeyOnly);
        });
    }

    /// A typo here is a silent policy downgrade if it were ever accepted as
    /// the permissive default instead of refused -- this is a compliance
    /// control (wheel-harness-auth.md), so an unrecognised value is a boot
    /// failure, the same posture `tool_allow_hosts` takes with a bad entry.
    #[test]
    fn an_unrecognised_harness_auth_value_is_a_boot_failure_not_a_silent_default() {
        for bad in ["api-key", "apikeyonly", "oauth", "OAUTH-TOKEN"] {
            with_env(&[(ENV_HARNESS_AUTH, Some(bad))], || {
                let err = harness_auth().unwrap_err();
                assert!(matches!(err, ConfigError::BadHarnessAuth(_)), "{err}");
                assert!(err.to_string().contains(bad), "{err}");
            });
        }
    }
}
