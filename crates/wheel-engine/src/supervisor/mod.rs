// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

//! The agent supervisor: one actor per agent node, owning its child process.
//!
//! Three defects observed running a real agent team on YOKE are designed out
//! here, and each has a test:
//!
//! * **§3c#13 — one process per agent, ever.** A message never spawns anything;
//!   it enqueues. `start` is idempotent and holds a per-agent lock across the
//!   spawn, so N quick messages cannot become N processes of one agent editing
//!   one worktree at once.
//! * **§3c#12 — a single stdin writer.** [`Supervisor`] owns the child's stdin
//!   handle and nothing else can reach it, so an operator's typed message and
//!   inbound agent traffic cannot interleave mid-turn.
//! * **F008 — forged harness events.** An agent controls its own stdout, so a
//!   `result` is honoured only when its `session_id` matches the session this
//!   supervisor started.

use std::{
    collections::HashMap,
    process::Stdio,
    sync::{Arc, Mutex},
};

use anyhow::{Context, Result};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin},
    sync::Mutex as AsyncMutex,
};
use uuid::Uuid;
use wheel_core::{AgentStatus, MessageState, NodeType};

use crate::{
    config::Config,
    db::{board, messages},
    harness::{claude::ClaudeDriver, Harness, HarnessEvent, SpawnSpec, StartupFailure},
};

/// Toolchain caches redirected from a node's `$HOME` to the project's own.
///
/// The variable is what the tool reads; the directory is where we put it. Each
/// one of these defaults to somewhere under `$HOME`, and `$HOME` is per node,
/// so without this every agent on a board downloads and stores its own copy of
/// the same packages.
///
/// `RUSTUP_HOME` is deliberately NOT here: the toolchain itself is in the
/// image, read-only and shared by every tenant already (029). This list is for
/// what a tenant FETCHES, which is theirs and must not be another tenant's.
const TOOL_CACHES: &[(&str, &str)] = &[
    ("CARGO_HOME", ".cargo"),
    // pnpm's content-addressed store: the 882M-per-agent one.
    ("PNPM_HOME", ".pnpm"),
    ("XDG_DATA_HOME", ".local-share"),
    // npm keeps `_cacache` under this.
    ("NPM_CONFIG_CACHE", ".npm"),
    // Browser downloads, which are the next few hundred MB of the same shape.
    ("PLAYWRIGHT_BROWSERS_PATH", ".playwright"),
    ("PUPPETEER_CACHE_DIR", ".puppeteer"),
    ("UV_CACHE_DIR", ".uv"),
    ("PIP_CACHE_DIR", ".pip"),
];

/// How much of a child's stdout is kept for classifying why it died. Enough
/// for a CLI's error banner, small enough that a runaway child cannot grow it.
const STARTUP_OUTPUT_TAIL: usize = 8 * 1024;

/// How often a running `api-key-only` agent's credential is re-checked
/// between idle-park ticks (wheel-harness-auth.md, "PM ruling: mid-flow
/// enforcement window"). Bounds the window a self-provisioned OAuth
/// credential can run on to one tick of this instead of "for as long as the
/// process happens to stay alive" -- short enough that the control means
/// something, long enough not to be a busy-poll.
const HARNESS_AUTH_RECHECK_SECS: u64 = 60;

/// The last few KiB of a child's stdout, kept to explain why it died.
///
/// Whole LINES, not bytes. It was a `String` trimmed with
/// `tail.drain(..drop_to)` where `drop_to` came from arithmetic on lengths --
/// and draining a `String` at an index that is not a character boundary
/// panics. That is the same defect as the envelope escaper, one layer down and
/// with a worse blast radius: it runs on the supervisor's stdout reader, so a
/// child whose output happens to put a multi-byte character at the cut loses
/// its start with nothing in the log to say why. Our own agents write em
/// dashes constantly.
///
/// Keeping lines removes the offset arithmetic rather than correcting it. The
/// unit of this buffer was always the line -- it is stdout being read
/// line-by-line -- and reasoning about it in bytes was the mistake, not the
/// particular index.
#[derive(Default)]
struct StartupTail {
    lines: std::collections::VecDeque<String>,
    bytes: usize,
}

impl StartupTail {
    fn push(&mut self, line: String) {
        self.bytes += line.len() + 1;
        self.lines.push_back(line);
        // Drop whole lines until it fits. A single line longer than the whole
        // budget leaves one line: an over-long banner is still the best
        // evidence we have of why the child died.
        while self.bytes > STARTUP_OUTPUT_TAIL && self.lines.len() > 1 {
            if let Some(dropped) = self.lines.pop_front() {
                self.bytes -= dropped.len() + 1;
            }
        }
    }

    fn into_string(self) -> String {
        let mut out = String::with_capacity(self.bytes);
        for line in self.lines {
            out.push_str(&line);
            out.push('\n');
        }
        out
    }
}

pub mod awaits;
pub mod git_creds;
mod prompt;
mod quota;
mod refresh;
pub mod workspace;
pub use prompt::compose_prompt;

/// What the supervisor knows about one running agent.
struct Running {
    /// Identifies THIS spawn. A child's reaper must not settle a slot that
    /// already holds its replacement — which is exactly what happens when an
    /// ephemeral turn restarts the session the moment the old child dies.
    run_id: Uuid,
    session_id: Option<String>,
    stdin: ChildStdin,
    child: Child,
    /// The agent leads its own process group (see `start`), so this is also the id of everything
    /// it spawned. Kept apart from `child.id()`, which is gone once the leader is reaped.
    pgid: Option<libc::pid_t>,
    /// The message currently occupying the child, if any. Exactly one at a
    /// time: the next is written only after this turn's `result`.
    in_flight: Option<Uuid>,
    /// Consecutive user-lane deliveries, for the §3 fairness cap.
    consecutive_user: u32,
    /// The harness reports turns and cost CUMULATIVELY for the session, so the
    /// last figures seen are kept here and only the DELTA is added to the
    /// agent's running totals. Summing the reports directly would make a
    /// board's `turns` a triangular series — three turns would read as six.
    counted_turns: u64,
    counted_usd: f64,
    /// The vault login this child was started on, if it runs on one. A renewal
    /// since then means it holds a token on its way out.
    credential_gen: Option<refresh::CredentialGen>,
    /// A `rejected` rate-limit event seen during the current turn, already
    /// matched to this session. Taken when the turn's `result` arrives.
    limit_seen: Option<quota::LimitSignal>,
    /// This spawn runs on the agent's `fallback_vault` credential.
    on_fallback: bool,
}

/// How long an agent gets to exit on SIGTERM when the engine shuts down, before SIGKILL.
const SHUTDOWN_GRACE: std::time::Duration = std::time::Duration::from_secs(3);

/// How long turns already running get to finish when the engine shuts down. With the SIGTERM grace
/// and the engine's HTTP drain it fits inside the 30 s a container is given to stop.
const SHUTDOWN_DRAIN: std::time::Duration = std::time::Duration::from_secs(20);

/// What a turn still running when the drain ran out is consumed with. Never requeued: see
/// [`Supervisor::shutdown`].
pub const INTERRUPTED_BY_SHUTDOWN: &str = "interrupted by engine shutdown";

/// What a turn is consumed with when [`Supervisor::interrupt`] cancels it. Never requeued, for the
/// same reason as [`INTERRUPTED_BY_SHUTDOWN`].
pub const INTERRUPTED_BY_USER: &str = "interrupted at the caller's request";

impl Running {
    /// SIGKILL the agent's whole process group, then reap the leader.
    ///
    /// The group, not the leader: an agent's tool shells and MCP servers are its descendants, and
    /// killing only the process we spawned orphaned every one of them.
    async fn kill(&mut self) -> std::io::Result<()> {
        self.signal_group(libc::SIGKILL);
        match self.child.try_wait() {
            Ok(Some(_)) => Ok(()),
            _ => self.child.kill().await,
        }
    }

    fn signal_group(&self, signal: libc::c_int) {
        if let Some(pgid) = self.pgid {
            // SAFETY: killpg only delivers a signal. The id is our child's own group.
            unsafe { libc::killpg(pgid, signal) };
        }
    }
}

/// One agent's slot. `None` means "not running"; the mutex is held ACROSS the
/// spawn, which is what collapses concurrent starts into one process (§3c#13)
/// rather than letting them race.
type AgentSlot = Arc<AsyncMutex<Option<Running>>>;

/// Every agent's slot, behind its own lock so one slow spawn cannot block
/// delivery to a different agent.
type AgentSlots = Arc<AsyncMutex<HashMap<Uuid, AgentSlot>>>;

/// Environment variables a child inherits from the engine when they are set.
///
/// Every one of these describes the MACHINE, not the project: where binaries
/// live, what locale and timezone to use, where scratch files go, and which
/// CA bundle to trust. None is a secret, and the harness cannot run without
/// at least `PATH`. Everything else the engine holds is dropped.
const INHERITED_ENV: &[&str] = &[
    "PATH",
    "LANG",
    "LC_ALL",
    "TZ",
    "TMPDIR",
    // A container with a private CA is unreachable without these, and the
    // failure would look like a network fault rather than a missing variable.
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "NODE_EXTRA_CA_CERTS",
    // Where the machine's shared, read-only Rust toolchain lives. A path, not
    // a secret -- the same class as PATH.
    //
    // Dropping this is what broke the first Wheel-on-Wheel run: the agent
    // cloned the repo and then could not build, with "rustup could not choose
    // a version of cargo to run". The image installs a default toolchain and
    // it was fine; clearing the environment for F015 took away the variable
    // that says where it is, and rustup fell back to a $HOME that has no
    // settings.toml. CARGO_HOME survived only because the supervisor sets it
    // per project explicitly.
    "RUSTUP_HOME",
];

/// Where to look for the harness when the engine itself was started without a
/// `PATH`. Matches what the host uses for the engine.
const DEFAULT_PATH: &str = "/usr/local/bin:/usr/bin:/bin";

/// The ONLY way this engine starts a child process.
///
/// F015 was a single missing `env_clear` at a single spawn site. The fix is
/// two lines, which is exactly why it must not live in each caller's
/// discipline: scripts and MCP servers arrive in M2, and a third spawn site
/// that forgot them would hand the engine's secrets to untrusted code again
/// with nothing to catch it. Building the clear into the constructor means a
/// child that skips it cannot be built.
///
/// A process can always read its own `/proc/self/environ`, so anything
/// inherited here is readable by untrusted code whatever uid it runs as.
/// Enforced by `every_child_process_is_started_through_child_command`.
/// Write the harness's MCP config: one stdio server, `wheel mcp-serve`.
///
/// The engine URL and the token FILE are passed as env on the server entry
/// rather than baked into the prompt or an argument, so the token never
/// reaches a command line (§5b: argv is world-readable across uids).
fn write_mcp_config(
    run_dir: &std::path::Path,
    token_file: &std::path::Path,
) -> Result<std::path::PathBuf> {
    let path = run_dir.join("mcp.json");
    let config = serde_json::json!({
        "mcpServers": {
            "wheel": {
                "type": "stdio",
                "command": "wheel",
                "args": ["mcp-serve"],
                "env": {
                    wheel_core::spawn::ENV_TOKEN_FILE: token_file.display().to_string(),
                },
            }
        }
    });
    std::fs::write(&path, serde_json::to_string_pretty(&config)?)
        .context("writing the mcp config")?;
    Ok(path)
}

pub(crate) fn child_command(program: impl AsRef<std::ffi::OsStr>) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(program);
    cmd.env_clear();
    inherit_platform_env(&mut cmd);
    cmd
}

fn inherit_platform_env(cmd: &mut tokio::process::Command) {
    for key in INHERITED_ENV {
        if let Some(value) = std::env::var_os(key) {
            cmd.env(key, value);
        }
    }
    if std::env::var_os("PATH").is_none() {
        cmd.env("PATH", DEFAULT_PATH);
    }
}

/// Said when a project has no vault key at all — a provisioning gap in
/// whatever spawned this engine, not something the caller did wrong.
pub const NO_VAULT_KEY: &str =
    "this engine started without WHEEL_VAULT_KEY, so secrets cannot be stored or read";

/// Said when the key is present but not a key. Different cause, different fix,
/// so it must not collapse into the message above.
pub const BAD_VAULT_KEY: &str =
    "this engine started with an unusable WHEEL_VAULT_KEY (expected base64 of 32 bytes), \
     so secrets cannot be stored or read";

pub struct Supervisor {
    cfg: Arc<Config>,
    /// Parsed once at construction: a project with an unusable vault key
    /// should fail loudly at boot, not on the first secret read.
    vault_key: Option<crate::vault::VaultKey>,
    vault_key_error: Option<&'static str>,
    db: Arc<Mutex<rusqlite::Connection>>,
    agents: AgentSlots,
    harness: Arc<dyn Harness>,
    events: Arc<crate::events::Bus>,
    /// Set when shutdown begins: from then on nothing starts and no new message is written.
    closing: AtomicBool,
    shutdown_drain_ms: AtomicU64,
    pub(crate) broker: refresh::Broker,
    /// Bumped by every `stop`. `start` releases the slot across a renewal, so
    /// without this a stop that lands in that window takes the empty slot, sets
    /// `stopped`, and the renewal then spawns a child anyway: the operator's
    /// stop returns success and the agent keeps running.
    stops: Mutex<HashMap<Uuid, u64>>,
    /// Who is blocked awaiting a reply from whom (parity proposal §2.5).
    awaits: Arc<Mutex<awaits::AwaitGraph>>,
    /// Spread, in seconds, over which agents return after a window reset.
    limit_jitter: std::ops::RangeInclusive<u64>,
}

impl Supervisor {
    pub fn new(
        cfg: Arc<Config>,
        db: Arc<Mutex<rusqlite::Connection>>,
        events: Arc<crate::events::Bus>,
    ) -> Self {
        Self::with_harness(cfg, db, events, Arc::new(ClaudeDriver))
    }

    /// Build a supervisor driving a specific harness. The seam that lets tests
    /// exercise real spawn/exit paths against a stub binary.
    pub fn with_harness(
        cfg: Arc<Config>,
        db: Arc<Mutex<rusqlite::Connection>>,
        events: Arc<crate::events::Bus>,
        harness: Arc<dyn Harness>,
    ) -> Self {
        // Said at boot, not discovered later from a failed write: a missing
        // key is a provisioning gap in whoever spawned this engine, and the
        // person who can fix it is reading the startup log.
        let (vault_key, vault_key_error) = match cfg.vault_key.as_deref() {
            None => {
                tracing::warn!(
                    "WHEEL_VAULT_KEY is not set; vault nodes will refuse reads and writes"
                );
                (None, Some(NO_VAULT_KEY))
            }
            Some(raw) => match crate::vault::VaultKey::from_base64(raw) {
                Ok(k) => (Some(k), None),
                Err(e) => {
                    tracing::error!(
                        error = %e,
                        "WHEEL_VAULT_KEY is unusable; vault nodes will refuse reads and writes"
                    );
                    (None, Some(BAD_VAULT_KEY))
                }
            },
        };
        Self {
            cfg,
            vault_key,
            vault_key_error,
            db,
            agents: Arc::new(AsyncMutex::new(HashMap::new())),
            harness,
            events,
            closing: AtomicBool::new(false),
            shutdown_drain_ms: AtomicU64::new(SHUTDOWN_DRAIN.as_millis() as u64),
            broker: refresh::Broker::default(),
            stops: Mutex::new(HashMap::new()),
            awaits: Arc::default(),
            limit_jitter: quota::JITTER_SECS,
        }
    }

    /// How many times this agent has been stopped. Read before a start gives
    /// up the slot and again when it takes it back.
    fn stop_epoch(&self, agent: Uuid) -> u64 {
        *self.stops.lock().unwrap().entry(agent).or_insert(0)
    }

    fn note_stop(&self, agent: Uuid) {
        *self.stops.lock().unwrap().entry(agent).or_insert(0) += 1;
    }

    /// Tests resume at the reset itself rather than up to a minute after it.
    #[cfg(test)]
    fn with_limit_jitter(mut self, jitter: std::ops::RangeInclusive<u64>) -> Self {
        self.limit_jitter = jitter;
        self
    }

    fn startup_deadline(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.cfg.startup_deadline_secs)
    }

    /// The project's vault key, if it has a usable one.
    pub fn vault_key(&self) -> Option<&crate::vault::VaultKey> {
        self.vault_key.as_ref()
    }

    /// The project's vault key, or the reason there isn't one.
    ///
    /// Callers get a sentence naming the missing environment variable rather
    /// than a bare failure: without it, a provisioning gap arrives as a 500
    /// and gets debugged as an engine bug.
    pub fn require_vault_key(&self) -> Result<&crate::vault::VaultKey, &'static str> {
        self.vault_key
            .as_ref()
            .ok_or(self.vault_key_error.unwrap_or(NO_VAULT_KEY))
    }

    /// The project's private crate cache, created 0700 and checked.
    ///
    /// QA's BUG-021 against 029: the comment beside this said the right thing
    /// and the code landed one level too high. `create_dir_all` uses the
    /// default mode, so the directory came out 0755 — readable by every OTHER
    /// uid in the sandbox, and §2 gives each agent, script and MCP child its
    /// own uid precisely so they are not each other's. What sits in there is
    /// downloaded sources and, the moment a tenant configures a private
    /// registry, `credentials.toml` with a token.
    ///
    /// The mode is SET rather than left to the umask, and then verified: a
    /// directory that already existed keeps whatever mode it was made with,
    /// so creating it correctly is not the same as finding it correct.
    fn project_cache(&self, name: &str) -> Result<std::path::PathBuf> {
        use std::os::unix::fs::PermissionsExt;

        // `.cargo` under the project's own data dir. On the process backend
        // that dir IS /data/projects/<id>; on docker it is the project's own
        // volume. Either way it is per-project, which is the property that
        // matters -- the previous name `cargo` also sat next to whatever else
        // shares a host data dir.
        let dir = self.cfg.data_dir.join(name);
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("creating the {name} cache at {}", dir.display()))?;

        // Before the mode, because every check below follows symlinks
        // (QA's WOW-toolchain-cargo-owned). A `.cargo` symlinked into a
        // shared location satisfies a path check AND a mode check -- the mode
        // read belongs to the target -- while every project quietly shares one
        // cache. `symlink_metadata` is the only call here that does not follow.
        let link = std::fs::symlink_metadata(&dir)
            .with_context(|| format!("inspecting the {name} cache at {}", dir.display()))?;
        anyhow::ensure!(
            link.is_dir(),
            "the {name} cache at {} is a {}, not a directory of this project's own; \
             a child is not started with it",
            dir.display(),
            if link.file_type().is_symlink() {
                "symlink"
            } else {
                "file"
            }
        );

        std::fs::set_permissions(&dir, PermissionsExt::from_mode(0o700))
            .with_context(|| format!("restricting {} to this project", dir.display()))?;

        // Belt: refuse to start rather than hand a child a cache another uid
        // can read. A registry token in a world-readable directory is worth
        // failing loudly over.
        let mode = std::fs::metadata(&dir)?.permissions().mode() & 0o077;
        anyhow::ensure!(
            mode == 0,
            "the {name} cache at {} is readable or writable by other uids (mode {:o}); \
             it holds fetched packages and any registry credentials, so a child is not \
             started with it",
            dir.display(),
            std::fs::metadata(&dir)?.permissions().mode() & 0o777
        );
        Ok(dir)
    }

    /// Root for everything a build or a script produces that is not worth
    /// keeping: cargo's `target/`, `$TMPDIR`. Off `/data` on purpose.
    ///
    /// `/data` is the one persistent volume, sized for `wheel.db` and the
    /// project's own state -- not for a `cargo build --workspace`'s object
    /// files, which run to gigabytes and are trivially reproducible. Every
    /// agent independently discovering this by hitting ENOSPC and pointing
    /// its own `CARGO_TARGET_DIR` at `/tmp` (as happened live on wheel-dev,
    /// more than once) is the same fix done N times instead of once, here.
    fn scratch_root(&self) -> std::path::PathBuf {
        std::path::PathBuf::from("/tmp/wheel-scratch").join(self.cfg.project_id.to_string())
    }

    /// Where a spawned child's `CARGO_TARGET_DIR` points. Per-project, not
    /// per-agent: cargo's own file locking already serialises concurrent
    /// builds against one target dir, and a build cache shared across this
    /// project's agents is the point (a second agent's build reuses the
    /// first's compiled dependencies instead of starting cold).
    fn cargo_target_dir(&self) -> Result<std::path::PathBuf> {
        Self::private_scratch_dir(
            self.scratch_root().join("cargo-target"),
            "build scratch dir",
        )
    }

    /// Where a spawned child's `TMPDIR` points, for the same reason as
    /// [`Self::cargo_target_dir`]: build tooling (and plenty else) writes
    /// temp files there, and the default is `/tmp` on a real machine but
    /// whatever `$TMPDIR` says otherwise -- pin it off `/data` explicitly
    /// rather than hope nothing has overridden it.
    fn tmp_dir(&self) -> Result<std::path::PathBuf> {
        Self::private_scratch_dir(self.scratch_root().join("tmp"), "scratch tmp dir")
    }

    /// Create (or find) `dir`, make it 0700, and refuse it if it turns out to
    /// be a symlink or if the mode does not stick -- the same hardening
    /// [`Self::project_cache`] applies to the registry cache, because the
    /// threat is identical: every agent, script and MCP child in this project
    /// has its own uid (§2), and a scratch dir any of them can read is not
    /// private to the one that is supposed to own it.
    fn private_scratch_dir(dir: std::path::PathBuf, what: &str) -> Result<std::path::PathBuf> {
        use std::os::unix::fs::PermissionsExt;

        std::fs::create_dir_all(&dir)
            .with_context(|| format!("creating the {what} at {}", dir.display()))?;

        let link = std::fs::symlink_metadata(&dir)
            .with_context(|| format!("inspecting the {what} at {}", dir.display()))?;
        anyhow::ensure!(
            link.is_dir(),
            "the {what} at {} is a {}, not a directory of this project's own; \
             a child is not started with it",
            dir.display(),
            if link.file_type().is_symlink() {
                "symlink"
            } else {
                "file"
            }
        );

        std::fs::set_permissions(&dir, PermissionsExt::from_mode(0o700))
            .with_context(|| format!("restricting {} to this project", dir.display()))?;

        let mode = std::fs::metadata(&dir)?.permissions().mode() & 0o077;
        anyhow::ensure!(
            mode == 0,
            "the {what} at {} is readable or writable by other uids (mode {:o}); \
             a child is not started with it",
            dir.display(),
            std::fs::metadata(&dir)?.permissions().mode() & 0o777
        );
        Ok(dir)
    }

    /// Why this agent cannot start on the credential it holds, if that is
    /// already known.
    ///
    /// Only an expiry the store actually recorded counts. A credential with no
    /// recorded expiry is treated as usable -- "we were not told" is not the
    /// same as "it has lapsed", and refusing to start on a guess would strand
    /// an agent whose credential is fine.
    fn lapsed_credential(&self, agent: Uuid, harness: wheel_core::Harness) -> Option<String> {
        let conn = self.db.lock().ok()?;
        let (vault, key, expires_at) =
            crate::vault::credential_detail(&conn, agent, harness).ok()??;
        // A refreshable login's expiry is a renewal deadline, not a lapse:
        // `start` renews it instead of refusing on it.
        if key == wheel_core::CLAUDE_OAUTH_SESSION {
            return None;
        }
        let expires_at = expires_at?;
        (expires_at.into_inner() <= time::OffsetDateTime::now_utc()).then(|| {
            format!(
                "the credential from vault {vault} expired at {expires_at}; \
                 sign in again, or store a `claude setup-token` token which does not expire"
            )
        })
    }

    /// Does a wired vault hand this agent an OAuth credential of any shape: a
    /// refreshable login, a `CLAUDE_CODE_OAUTH_TOKEN`, or an `sk-ant-oat` value
    /// under any other name (an agent can export any key it can read)?
    fn vault_supplies_oauth(&self, agent: Uuid) -> bool {
        let conn = self.db.lock().unwrap();
        if crate::vault::session_vault_for(&conn, agent)
            .ok()
            .flatten()
            .is_some()
        {
            return true;
        }
        let Some(vk) = self.vault_key() else {
            return false;
        };
        crate::vault::env_for_agent(&conn, vk, agent).is_ok_and(|env| {
            env.iter().any(|(k, v)| {
                k == "CLAUDE_CODE_OAUTH_TOKEN"
                    || crate::auth::classify_token(v, wheel_core::Harness::Claude)
                        == wheel_core::CredentialKind::OauthToken
            })
        })
    }

    pub fn events(&self) -> &Arc<crate::events::Bus> {
        &self.events
    }

    async fn slot(&self, agent: Uuid) -> Arc<AsyncMutex<Option<Running>>> {
        let mut map = self.agents.lock().await;
        map.entry(agent)
            .or_insert_with(|| Arc::new(AsyncMutex::new(None)))
            .clone()
    }

    /// Start an agent. **Idempotent**: starting one that is already running is
    /// a no-op that returns the existing session (§3c#13).
    pub async fn start(self: &Arc<Self>, agent: Uuid) -> Result<AgentStatus> {
        let slot = self.slot(agent).await;
        let stops_before = self.stop_epoch(agent);
        {
            // Checked under the slot lock: shutdown takes every slot after setting the flag, so a
            // start either finishes first and is stopped with the rest, or sees the flag and refuses.
            let guard = slot.lock().await;
            if self.closing.load(Ordering::SeqCst) {
                anyhow::bail!("the engine is shutting down; nothing starts now");
            }
            if let Some(r) = guard.as_ref() {
                // Already running. Do NOT spawn a second process.
                let _ = r.session_id;
                return Ok(AgentStatus::Running);
            }
        }

        let (node, resume, fallback_until) = {
            let conn = self.db.lock().unwrap();
            let node =
                board::get(&conn, agent)?.ok_or_else(|| anyhow::anyhow!("no such node {agent}"))?;
            anyhow::ensure!(
                node.node_type() == NodeType::Agent,
                "{} is not an agent node",
                node.name
            );
            // Idle parking keeps the session id so a resume is transparent.
            let state = board::agent_state(&conn, agent).unwrap_or_default();
            (node, state.session_id, state.fallback_until)
        };

        let agent_cfg = node
            .config
            .as_agent()
            .ok_or_else(|| anyhow::anyhow!("not an agent config"))?
            .clone();

        // The harness field picks credentials, never the binary: the driver is
        // a hardcoded ClaudeDriver. Starting a codex node would therefore spawn
        // `claude` with codex credentials and never say so — a silent harness
        // substitution the operator cannot see. Refuse instead, and say why.
        if !crate::harness::has_driver(agent_cfg.harness) {
            let h = agent_cfg.harness;
            let reason = format!(
                "harness \"{h}\" is not supported by this build (M2): there is no {h} \
                 driver, so starting this node would silently run claude instead"
            );
            self.set_status(agent, AgentStatus::Error, Some(reason));
            return Ok(AgentStatus::Error);
        }

        // A credential that has already lapsed fails on the child's first
        // request, and the harness reports that as its own confusing error --
        // so the operator sees a broken agent rather than one that needs a
        // login. Checking before we spawn turns it into the one status they
        // can act on, without burning a process to discover it.
        if let Some(reason) = self.lapsed_credential(agent, agent_cfg.harness) {
            self.set_status(agent, AgentStatus::NeedsAuth, Some(reason));
            return Ok(AgentStatus::NeedsAuth);
        }

        // docs/proposals/wheel-harness-auth.md, enforcement point 4: the gate
        // that actually holds. `auth/begin`/`auth/complete`/vault `PUT`
        // (api/agent_routes.rs, api/vault_routes.rs) refuse an OAuth-shaped
        // credential before it is ever
        // stored, but an agent is untrusted code with a shell and can write
        // one itself (`claude auth login`/`claude setup-token`) without going
        // through any of those routes. Spawn is the one point that inspects
        // the credential the harness is actually about to run with, so it is
        // the point that cannot be bypassed.
        if self.cfg.harness_auth == crate::config::HarnessAuthPolicy::ApiKeyOnly {
            let config_dir = self.cfg.creds_dir().join(agent.to_string());
            if crate::auth::is_oauth_shaped(&config_dir, agent_cfg.harness)
                || self.vault_supplies_oauth(agent)
            {
                let reason = "this project is api-key-only: an OAuth-shaped credential is not \
                               permitted here, store an API key instead"
                    .to_string();
                self.set_status(agent, AgentStatus::Error, Some(reason));
                return Ok(AgentStatus::Error);
            }
        }

        // A refreshable vault login is renewed BEFORE the child starts if it is
        // near its end, so no child is ever handed a token about to die. If it
        // cannot be renewed and has already lapsed, this is `needs_auth` with
        // the reason, and the message that woke the agent stays queued.
        let session_vault = {
            let conn = self.db.lock().unwrap();
            crate::vault::session_vault_for(&conn, agent).ok().flatten()
        };
        // Deliberately NOT under the slot lock: renewing runs the CLI on a
        // 60-second budget, and this agent's slot is taken for every delivery
        // to it -- and `live_agents` walks every slot for `/healthz`. Holding
        // it across the renewal made one agent's renewal everyone's problem.
        let credential_gen = match session_vault {
            Some(vault) => {
                if let Err(reason) = self.ensure_fresh(vault, None).await {
                    self.set_status(agent, AgentStatus::NeedsAuth, Some(reason));
                    return Ok(AgentStatus::NeedsAuth);
                }
                Some((vault, self.session_generation(vault)))
            }
            None => None,
        };

        // Re-taken for the spawn itself, which is what §3c#13 requires: one
        // process per agent, decided under this lock. A start that won the race
        // while we were renewing has already spawned on the same fresh login.
        let mut guard = slot.lock().await;
        if self.closing.load(Ordering::SeqCst) {
            anyhow::bail!("the engine is shutting down; nothing starts now");
        }
        if guard.is_some() {
            return Ok(AgentStatus::Running);
        }
        // Someone stopped this agent while the renewal ran. They took an empty
        // slot and got `stopped`; spawning now would hand them a running agent
        // they just stopped, with a token minted after they stopped it.
        if self.stop_epoch(agent) != stops_before {
            tracing::info!(%agent, "a stop landed while this start was renewing its login; not spawning");
            return Ok(AgentStatus::Stopped);
        }

        let run_dir = self.cfg.node_run_dir(agent);
        std::fs::create_dir_all(&run_dir)?;
        // The agent's own working copy (§3e), not the data root. See
        // `Config::workspace_dir` for what this does and does not fix.
        let workspace = self.cfg.workspace_dir(node.name.as_str());

        // Finding 036: a live GitHub PAT was found in `.git/config` on the
        // production volume, because an agent cloned with the token in the
        // remote URL. Clones made before this ran keep that token for ever, so
        // repairing them is part of starting, not a migration someone
        // remembers to run.
        match git_creds::sanitise_remotes(&workspace) {
            Ok(0) => {}
            Ok(n) => tracing::warn!(
                node = %node.name,
                remotes = n,
                "removed credentials from git remote URLs in this workspace; rotate those tokens"
            ),
            Err(e) => tracing::warn!(node = %node.name, error = %e, "could not check git remotes"),
        }
        std::fs::create_dir_all(&workspace).with_context(|| {
            format!(
                "creating the workspace for {} at {}",
                node.name,
                workspace.display()
            )
        })?;

        // §3e `workspaces`, tickets A9/A8. Until this ran, agents improvised
        // their own clones — which is how a live PAT reached `.git/config`, and
        // how three agents' full copies of one repository filled a 4.6 GB
        // volume. The credential goes through the askpass helper's environment
        // and every agent on a repo shares one object store.
        let git_token = {
            let conn = self.db.lock().unwrap();
            self.vault_key().and_then(|vk| {
                crate::vault::env_for_agent(&conn, vk, agent)
                    .ok()
                    .and_then(|env| {
                        env.into_iter()
                            .find(|(k, _)| k == "GITHUB_TOKEN" || k == "GH_TOKEN")
                            .map(|(_, v)| v)
                    })
            })
        };
        let materialised = workspace::materialise(
            &self.cfg.data_dir,
            &workspace,
            &run_dir,
            &agent_cfg.workspaces,
            git_token.as_deref(),
        )
        .await
        .unwrap_or(workspace::Materialised {
            cwd: None,
            failures: Vec::new(),
        });
        // The only other await in this guarded span (ADVERSARY, tracing the
        // same lock #64 added the check above for): `stop`'s `note_stop` bumps
        // the epoch before it ever needs the slot lock, same as the renewal
        // case, but a stop landing HERE would otherwise sail past a check that
        // already happened before this await started. Without this, `stop`
        // still wins eventually -- it blocks on the slot lock until this
        // function stores `Running` and kills it a moment later -- but only
        // after a process was spawned and a token was minted for an agent the
        // operator had already told to stop, exactly what the comment above
        // says the check exists to prevent.
        if self.stop_epoch(agent) != stops_before {
            tracing::info!(%agent, "a stop landed while this start was materialising its workspace; not spawning");
            return Ok(AgentStatus::Stopped);
        }
        // The agent still starts without a workspace it could not have — but on
        // its own log stream, next to whatever it does next, rather than only in
        // an engine log nobody is reading during a wake.
        if !materialised.failures.is_empty() {
            let conn = self.db.lock().unwrap();
            for failure in &materialised.failures {
                log_line_bus(&self.events, &conn, agent, "engine", failure);
            }
        }
        // Kept before the move: WHEEL_WORKSPACE is set only when a workspace was
        // really materialised, so it never names the fallback directory.
        let materialised_ws = materialised.cwd.clone();
        let cwd = materialised.cwd.unwrap_or_else(|| workspace.clone());
        let config_dir = self.cfg.creds_dir().join(agent.to_string());
        std::fs::create_dir_all(&config_dir)?;

        // The composed prompt goes to a file, never argv.
        let prompt = {
            let conn = self.db.lock().unwrap();
            compose_prompt(&conn, &node, &self.cfg.project_id.to_string())?
        };
        let prompt_file = run_dir.join("prompt.txt");
        std::fs::write(&prompt_file, prompt).context("writing the composed prompt")?;

        // Mint the node's capability token and hand it over as a 0600 FILE.
        // Rotating here bounds a leaked token to one run, and a file rather
        // than an env var keeps it out of /proc/<pid>/environ, which any
        // process of the same uid can read (ADVERSARY F007).
        let token_file = run_dir.join("token");
        {
            let conn = self.db.lock().unwrap();
            let minted = crate::db::tokens::mint(&conn, agent)?;
            write_secret_file(&token_file, &minted.plaintext)
                .context("writing the node token file")?;
        }

        // The board as MCP tools (§3c#1). Written per start, next to the
        // prompt, so a harness that reads it once at launch gets the current
        // shape -- and so the token file it points at is this run's.
        let mcp_config = write_mcp_config(&run_dir, &token_file)?;

        let spec = SpawnSpec {
            node_id: agent,
            node_name: node.name.to_string(),
            model: agent_cfg.model.clone(),
            prompt_file,
            mcp_config: Some(mcp_config),
            resume,
            config_dir,
            cwd,
        };

        self.set_status(agent, AgentStatus::Starting, None);
        let (fallback, use_fallback) = self.credential_plan(agent, &agent_cfg, fallback_until);

        let mut cmd = child_command(self.harness.program());
        cmd.args(self.harness.argv(&spec))
            .current_dir(&spec.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            // Its own process group, so stopping the agent can reach everything it started.
            .process_group(0);
        for (k, v) in self.harness.env(&spec) {
            cmd.env(k, v);
        }
        // How the child reaches its board. WHEEL_TOKEN is deliberately NOT set:
        // the CLI errors loudly if it finds one, so a future regression that
        // reintroduces an env token is noisy rather than silent.
        cmd.env(wheel_core::spawn::ENV_TOKEN_FILE, &token_file);
        cmd.env(
            wheel_core::spawn::ENV_ENGINE_URL,
            self.cfg.listen.client_url(),
        );
        cmd.env(wheel_core::spawn::ENV_NODE, node.name.as_str());
        // The cwd is already this path; the variable exists so an agent or a
        // script can NAME it without relying on not having moved. Set only when
        // a workspace was actually materialised — an empty value would read as
        // "there is one, it is nowhere".
        if let Some(ws) = materialised_ws.as_ref() {
            cmd.env(wheel_core::spawn::ENV_WORKSPACE, ws);
        }

        // Git authenticates from the ENVIRONMENT, so an agent never needs to
        // put a token in a remote URL — where it is written to disk — or on a
        // command line, where `/proc` publishes it to every other uid.
        match git_creds::write_askpass(&run_dir) {
            Ok(askpass) => {
                cmd.env("GIT_ASKPASS", &askpass);
                // Without this, git falls back to prompting on a tty that is
                // not there and the agent sees a hang rather than an error.
                cmd.env("GIT_TERMINAL_PROMPT", "0");
            }
            Err(e) => tracing::warn!(error = %e, "no git askpass helper for this child"),
        }

        // A private crate cache per project. The toolchain in the image is
        // shared and immutable; what a tenant FETCHES is not, and a shared
        // CARGO_HOME would put one project's downloaded sources -- and its
        // registry credentials, if it ever configures any -- where the next
        // project can read them.
        // Every toolchain cache an agent's tools would otherwise put in $HOME.
        //
        // $HOME is per NODE by design (F007: each child gets its own 0700
        // credential dir), which means every one of these downloads the same
        // bytes again per agent and nothing ever shares or reclaims them. API
        // measured the consequence twice on the production volume: 1.76G of
        // byte-identical pnpm store across two agents, after the same shape in
        // cargo had already helped fill a 4.6G disk and take the grid down.
        // Six agents on one board would be 5.3G of duplicates on that volume.
        //
        // So each is redirected to the PROJECT's cache, downloaded once and
        // shared by that tenant's agents. Per tenant, not per node — the same
        // reasoning M1.6 gives for CARGO_HOME and RUSTUP_HOME.
        for (var, dir) in TOOL_CACHES {
            match self.project_cache(dir) {
                Ok(path) => {
                    cmd.env(var, &path);
                }
                Err(e) => return Err(e),
            }
        }
        // Build output and temp files off the small persistent volume (PM
        // ruling, live ENOSPC + memory-pressure incident on wheel-dev): a
        // `cargo build --workspace` on `/data` is both the disk-full failure
        // mode and the biggest measured contributor to reported memory
        // pressure (page cache from that I/O counts against the project's
        // cgroup). Set for every agent, not left for each one to hit ENOSPC
        // and fix its own session. Unlike `TOOL_CACHES` above, this is build
        // OUTPUT, not a registry cache -- not worth keeping, so it lives off
        // `/data` entirely rather than under it.
        cmd.env("CARGO_TARGET_DIR", self.cargo_target_dir()?);
        cmd.env("TMPDIR", self.tmp_dir()?);
        // Stored credentials, if any. Absent is not an error: the harness may
        // hold OAuth credentials in its own config dir, and the authoritative
        // answer is its probe rather than our guess.
        //
        // Not on a fallback spawn: that runs as the fallback vault's account
        // and nothing else, so a token the agent planted in its own config dir
        // cannot ride along into it (credential-distribution rule).
        if !use_fallback {
            for (k, v) in crate::auth::credential_env(&spec.config_dir, agent_cfg.harness) {
                cmd.env(k, v);
            }
        }

        // Wired vaults, last, so a vault-supplied credential wins over a
        // pasted one: the vault is the thing the operator can see and change
        // on the board, and it is how a project runs several accounts.
        //
        // The third and final ambiguity check. The wire and the write are both
        // refused earlier, but only this one is guaranteed to run before a
        // child exists — a board restored from an export, or wires written
        // before this rule existed, reach here without passing either.
        let vault_env = match &self.vault_key {
            Some(vk) => {
                let conn = self.db.lock().unwrap();
                crate::vault::env_for_spawn(
                    &conn,
                    vk,
                    agent,
                    fallback.as_ref().map(|(id, _)| *id),
                    use_fallback,
                )?
            }
            None => Vec::new(),
        };
        let secrets: Vec<String> = vault_env.iter().map(|(_, v)| v.clone()).collect();
        for (k, v) in vault_env {
            cmd.env(k, v);
        }

        let mut child = cmd.spawn().context("spawning the harness")?;
        let stdin = child.stdin.take().expect("stdin was piped");
        let stdout = child.stdout.take().expect("stdout was piped");
        let stderr = child.stderr.take().expect("stderr was piped");

        let run_id = Uuid::new_v4();
        let pgid = child.id().and_then(|pid| libc::pid_t::try_from(pid).ok());
        *guard = Some(Running {
            run_id,
            session_id: None,
            stdin,
            child,
            pgid,
            in_flight: None,
            consecutive_user: 0,
            counted_turns: 0,
            counted_usd: 0.0,
            credential_gen,
            limit_seen: None,
            on_fallback: use_fallback,
        });
        drop(guard);

        // stderr is logged by its own task; the stdout task waits for it and
        // is the single owner of "this child has died", so the slot is cleared
        // and the status settled exactly once, in a defined order.
        let stderr_done = self.pump_stderr(agent, stderr, secrets.clone());
        self.clone()
            .pump_stdout(agent, stdout, slot.clone(), stderr_done, run_id, secrets);

        self.clone().watch_for_a_wedged_start(agent, run_id);

        Ok(AgentStatus::Starting)
    }

    /// Settle an agent that is still `starting` with work it cannot reach.
    ///
    /// ADVERSARY 041, with the correction that matters: the predicate is NOT
    /// "time in `starting`". Measured on production, the `pm` agent sat in
    /// `starting` for 52 minutes emitting nothing and went to `idle` the
    /// instant a message arrived — it was waiting correctly, and a flat
    /// deadline would have killed a healthy agent every minute forever. So an
    /// EMPTY QUEUE means there is nothing to be late for and this declines to
    /// judge, which is the same predicate QA's PROGRESS-deadline-outcome
    /// asserts: *an agent with work queued does not stay transitional past the
    /// deadline*.
    ///
    /// What it does when it fires is the other half, and PM's objection to a
    /// weaker version of this: killing and respawning the child would satisfy
    /// "resolves within 60s" while making the hang PERIODIC, which is worse
    /// than the bug. So this settles the agent into an answer — `error`, with a
    /// reason an operator can read — and spawns nothing. §3c#13 is not bent to
    /// meet a deadline.
    ///
    /// One timer per start, not a poll: it sleeps once, checks once, and is
    /// gone. `run_id` makes it inert against the run it was started for having
    /// already been replaced.
    fn watch_for_a_wedged_start(self: Arc<Self>, agent: Uuid, run_id: Uuid) {
        tokio::spawn(async move {
            tokio::time::sleep(self.startup_deadline()).await;

            let still_this_run = {
                let slot = self.slot(agent).await;
                let guard = slot.lock().await;
                guard.as_ref().map(|r| r.run_id) == Some(run_id)
            };
            if !still_this_run {
                return;
            }

            let (status, waiting) = {
                let conn = self.db.lock().unwrap();
                let status = board::agent_state(&conn, agent).unwrap_or_default().status;
                // A failed lookup must not read as "nothing queued": that would
                // silently decline to judge exactly when the board is unwell.
                let waiting = messages::has_queued(&conn, agent).unwrap_or(true);
                (status, waiting)
            };

            if status != AgentStatus::Starting || !waiting {
                return;
            }

            let secs = self.cfg.startup_deadline_secs;
            self.set_status(
                agent,
                AgentStatus::Error,
                Some(format!(
                    "the agent had messages queued but never finished starting within {secs}s, \
                     so nothing was delivered. Its process was left alone rather than replaced — \
                     restarting it here would hide the problem and spawn a second process for one \
                     agent. Check the agent's log for what the harness was doing."
                )),
            );
            tracing::error!(
                agent = %agent,
                deadline_secs = secs,
                "an agent with queued work never left `starting`; settled to error"
            );
        });
    }

    /// Stop an agent's child. Keeps the session id so a later start resumes.
    /// The agents this supervisor currently holds a live process for.
    ///
    /// Liveness lives here and nowhere else: the database records what was
    /// intended, this map records what is actually running. The stall report
    /// needs both to tell a turn in progress from a wedge.
    pub async fn live_agents(&self) -> std::collections::HashSet<Uuid> {
        // Slots that HOLD a process, not slots that exist. An agent keeps its
        // slot for ever once it has started one, so counting keys reported a
        // parked agent as live -- and `stalled_agents` reads this to tell a
        // turn in progress from a wedge, so the one state the system cannot
        // leave on its own was the one it could not see.
        //
        // The handles are cloned and the MAP lock released before any slot is
        // touched, and each slot is taken with `try_lock`. Both matter: a slot
        // can be held for as long as a spawn takes, and the map lock is what
        // every start, park and delivery needs. Awaiting a slot while holding
        // the map would let one `/healthz` freeze the whole board.
        let slots: Vec<(Uuid, AgentSlot)> = {
            let map = self.agents.lock().await;
            map.iter().map(|(id, slot)| (*id, slot.clone())).collect()
        };
        let mut live = std::collections::HashSet::new();
        for (agent, slot) in slots {
            // A slot we cannot take this instant is mid-spawn or mid-kill,
            // which is a process in transition rather than an absent one.
            let holding = match slot.try_lock() {
                Ok(guard) => guard.is_some(),
                Err(_) => true,
            };
            if holding {
                live.insert(agent);
            }
        }
        live
    }

    /// Stop an idle agent's process, keeping its session so the next message
    /// resumes it (§3c#14).
    ///
    /// This is the difference between Wheel and YOKE on compute. Measured on
    /// the production board before this existed: an agent an hour past its last
    /// turn still held a 162 MB `claude` process, because nothing ever stopped
    /// it. Six agents is a gigabyte of doing nothing.
    ///
    /// Parking is `stop` that keeps `session_id`: `deliver` resumes a parked
    /// agent when work arrives, and `--resume` continues the same session —
    /// proven in production, the first wake came back on the id it went to
    /// sleep with.
    async fn park(self: &Arc<Self>, agent: Uuid) -> Option<u64> {
        let slot = self.slot(agent).await;
        let mut guard = slot.lock().await;

        // Re-checked under the slot lock, because the timer and an arriving
        // message race by construction. Whichever takes the lock first wins and
        // the other sees the world it made: park first and `deliver` restarts
        // it; deliver first and this finds work queued and leaves it alone.
        let remaining = {
            let conn = self.db.lock().unwrap();
            let state = board::agent_state(&conn, agent).unwrap_or_default();
            if !matches!(state.status, AgentStatus::Idle) {
                return None;
            }
            if messages::has_queued(&conn, agent).unwrap_or(true) {
                return None;
            }

            // A timer is armed per turn and none is cancelled, so an OLD timer
            // outlives the turn that armed it: two turns 250s apart under a 300s
            // timeout leaves the first firing 50s after the second, parking an
            // agent that was active moments ago. Parking early is safe — it
            // resumes — but it silently shortens the hot window the operator
            // configured, so `idle_timeout_secs` would not mean what it says.
            //
            // Comparing against last_activity rather than cancelling timers is
            // QA's suggestion (BUG-040) and the better one: no extra state, and
            // it self-corrects however many stale timers are in flight.
            let idle_secs = Self::idle_timeout_for(&conn, agent);
            match Self::seconds_since_activity(&conn, agent) {
                Some(since) if since < idle_secs => Some(idle_secs - since),
                _ => None,
            }
        };
        // Reported, not re-armed here: re-arming by calling `park` again would
        // make it recursive, and a recursive async fn's future cannot be proven
        // Send. The caller's loop does the waiting instead.
        if let Some(left) = remaining {
            return Some(left);
        }

        let Some(mut r) = guard.take() else {
            // No process to stop: nothing to park, and marking it Parked would
            // claim a saving that was never made.
            return None;
        };
        {
            // NOT `let _ =`. A kill that fails leaves a live process while the
            // board says Parked — the saving is claimed and not made, and it is
            // invisible. `kill_on_drop(true)` still reaps `r` at the end of this
            // scope, so the process does die; what was missing was anyone ever
            // hearing that the direct kill did not work. (ADVERSARY, on the
            // risk I flagged as #2 in the review request.)
            if let Err(e) = r.kill().await {
                tracing::warn!(
                    %agent,
                    error = %e,
                    "killing a parked agent's process failed; kill_on_drop is the backstop"
                );
            }
        }
        {
            // Same reasoning as `stop`: a token outliving its process is a
            // credential with no owner. `start` mints a fresh one.
            let conn = self.db.lock().unwrap();
            let _ = crate::db::tokens::revoke(&conn, agent);
        }
        self.set_status(agent, AgentStatus::Parked, None);
        tracing::info!(%agent, "parked after idle timeout");
        None
    }

    /// The agent's configured idle timeout, or the default.
    fn idle_timeout_for(conn: &rusqlite::Connection, agent: Uuid) -> u64 {
        match board::get(conn, agent) {
            Ok(Some(node)) => match &node.config {
                wheel_core::NodeConfig::Agent(a) => a.idle_timeout_secs() as u64,
                _ => 0,
            },
            _ => 0,
        }
    }

    /// Seconds since the agent's `last_activity`, computed by sqlite.
    ///
    /// In SQL rather than in Rust because the timestamp is already stored as
    /// RFC3339 text and `julianday` is how every other age in this engine is
    /// measured — the stall report does the same. It also avoids adding a date
    /// library to a crate that has managed without one.
    fn seconds_since_activity(conn: &rusqlite::Connection, agent: Uuid) -> Option<u64> {
        conn.query_row(
            "SELECT CAST((julianday('now') - julianday(last_activity)) * 86400.0 AS INTEGER)
               FROM agent_state WHERE node_id = ?1 AND last_activity IS NOT NULL",
            rusqlite::params![agent.to_string()],
            |r| r.get::<_, i64>(0),
        )
        .ok()
        .map(|s| s.max(0) as u64)
    }

    /// Arm the idle timer for an agent that has just finished a turn.
    ///
    /// `idle_timeout_secs == 0` means never park — an agent that must stay
    /// hot -- UNLESS this project is `api-key-only`, in which case the timer
    /// still runs for the compliance re-check below even though it will
    /// never park (wheel-harness-auth.md, "PM ruling: mid-flow enforcement
    /// window": a busy agent that never naturally idles is exactly the case
    /// that ruling exists for).
    fn arm_park_timer(self: &Arc<Self>, agent: Uuid) {
        let secs = {
            let conn = self.db.lock().unwrap();
            match board::get(&conn, agent) {
                Ok(Some(node)) => match &node.config {
                    wheel_core::NodeConfig::Agent(a) => a.idle_timeout_secs(),
                    _ => return,
                },
                _ => return,
            }
        };
        let recheck_auth = self.cfg.harness_auth == crate::config::HarnessAuthPolicy::ApiKeyOnly;
        if secs == 0 && !recheck_auth {
            return;
        }
        let me = Arc::clone(self);
        tokio::spawn(async move {
            // The compliance re-check's own cadence, capped independently of
            // the configured idle timeout -- otherwise `idle_timeout_secs: 0`
            // ("never park") or a long one would leave the mid-flow window
            // open for as long as the process happens to stay busy, which is
            // the exact gap the ruling closes.
            let cap = if recheck_auth {
                HARNESS_AUTH_RECHECK_SECS
            } else {
                u64::MAX
            };
            let mut wait = if secs == 0 {
                cap
            } else {
                (secs as u64).min(cap)
            };
            // Loops rather than recurses, and re-waits whatever `park` reports
            // is left — so a stale timer from an earlier turn corrects itself
            // instead of parking an agent that was active moments ago.
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(wait.max(1))).await;

                if recheck_auth && me.kill_if_oauth_shaped(agent).await {
                    // Policy violation: killed. The next start re-runs the
                    // identical check at spawn and refuses the credential, so
                    // there is nothing further for this timer to do.
                    break;
                }

                match me.park(agent).await {
                    Some(left) => wait = left.max(1),
                    None if recheck_auth => {
                        // `park` declined -- busy, messages queued, or it
                        // already parked. Only the first two mean there is
                        // still a process to keep re-checking; once parked
                        // (or stopped) there is nothing left to watch.
                        let still_running = {
                            let conn = me.db.lock().unwrap();
                            matches!(
                                board::agent_state(&conn, agent).unwrap_or_default().status,
                                AgentStatus::Running | AgentStatus::Idle | AgentStatus::Starting
                            )
                        };
                        if !still_running {
                            break;
                        }
                        wait = cap;
                    }
                    None => break,
                }
            }
        });
    }

    /// Compliance re-check for `api-key-only` deployments (wheel-harness-
    /// auth.md, "PM ruling: mid-flow enforcement window"). A running agent
    /// can execute `claude auth login`/`codex login` inside its own turn --
    /// an agent is untrusted code with a shell (contract §2) -- and the CLI
    /// picks up that freshly-written credential immediately, in-process, with
    /// no restart. The spawn gate alone only re-checks on the NEXT start, so
    /// on a busy agent that never idles long enough to park that window is
    /// unbounded. This closes it: same detection [`crate::auth::is_oauth_shaped`]
    /// spawn already uses, same kill [`Self::park`] already does, just on a
    /// timer instead of at start.
    ///
    /// Returns whether it killed the process.
    async fn kill_if_oauth_shaped(self: &Arc<Self>, agent: Uuid) -> bool {
        let (harness, config_dir) = {
            let conn = self.db.lock().unwrap();
            match board::get(&conn, agent) {
                Ok(Some(node)) => match &node.config {
                    wheel_core::NodeConfig::Agent(a) => {
                        (a.harness, self.cfg.creds_dir().join(agent.to_string()))
                    }
                    _ => return false,
                },
                _ => return false,
            }
        };
        if !crate::auth::is_oauth_shaped(&config_dir, harness) {
            return false;
        }

        let slot = self.slot(agent).await;
        let mut guard = slot.lock().await;
        let Some(mut r) = guard.take() else {
            return false; // parked, stopped, or never started: nothing to kill
        };
        // NOT `let _ =` -- same reasoning as `park`: a kill that fails leaves
        // a live process while the board says otherwise. `kill_on_drop(true)`
        // still reaps `r` when it drops, so the process does die either way;
        // what matters is that a failure here is heard.
        if let Err(e) = r.kill().await {
            tracing::warn!(
                %agent,
                error = %e,
                "killing an api-key-only agent on an OAuth-shaped credential failed; \
                 kill_on_drop is the backstop"
            );
        }
        drop(guard);
        {
            // Same reasoning as `park`/`stop`: a token outliving its process
            // is a credential with no owner. `start` mints a fresh one, and
            // the spawn gate refuses it again before that ever happens.
            let conn = self.db.lock().unwrap();
            let _ = crate::db::tokens::revoke(&conn, agent);
        }
        self.set_status(
            agent,
            AgentStatus::Error,
            Some(
                "this project is api-key-only: an OAuth-shaped credential appeared on this \
                 node mid-turn (a native harness login run inside the agent's own session) \
                 and the process was stopped; store an API key instead"
                    .to_string(),
            ),
        );
        tracing::warn!(
            %agent,
            "stopped a running agent: OAuth-shaped credential found under api-key-only policy"
        );
        true
    }

    pub async fn stop(&self, agent: Uuid) -> Result<AgentStatus> {
        // Before the slot, so a start that is mid-renewal sees this even
        // though it holds nothing right now.
        self.note_stop(agent);
        let slot = self.slot(agent).await;
        let mut guard = slot.lock().await;
        if let Some(mut r) = guard.take() {
            // Same reasoning as `park`: kill_on_drop reaps it either way, but a
            // failure that nobody hears is a process the board thinks is gone.
            if let Err(e) = r.kill().await {
                tracing::warn!(%agent, error = %e, "killing a stopped agent's process failed");
            }
        }
        {
            // Revoke on stop: a token left live after the process is gone is a
            // credential with no owner.
            let conn = self.db.lock().unwrap();
            let _ = crate::db::tokens::revoke(&conn, agent);
        }
        self.set_status(agent, AgentStatus::Stopped, None);
        Ok(AgentStatus::Stopped)
    }

    /// `POST /v1/agents/:id/interrupt` (§3c#12, PROTOCOL.md M2): cancel the turn this agent is in
    /// the middle of, without losing its session -- `stop`'s smaller sibling, which also drops the
    /// ability to resume where the conversation was. A no-op, reporting the CURRENT status, when
    /// there is no turn in flight: an agent that is stopped or idle between turns has nothing to
    /// interrupt, and killing it anyway would make this route a second `stop` wearing this name.
    ///
    /// Same reasoning as [`Supervisor::shutdown`]'s drain-timeout branch: the cancelled turn is
    /// consumed with [`INTERRUPTED_BY_USER`] rather than requeued, because a turn killed mid-flight
    /// may already have committed or pushed and running it again would do that twice. The session
    /// is kept (`Parked`, not `Stopped`) and whatever is queued behind it -- typically the message
    /// the caller wanted to steer the agent with -- is delivered immediately after, resuming on the
    /// SAME session rather than starting a fresh one.
    pub async fn interrupt(self: &Arc<Self>, agent: Uuid) -> Result<AgentStatus> {
        let in_flight = {
            let slot = self.slot(agent).await;
            let mut guard = slot.lock().await;
            if !guard.as_ref().is_some_and(|r| r.in_flight.is_some()) {
                drop(guard);
                let conn = self.db.lock().unwrap();
                return Ok(board::agent_state(&conn, agent).unwrap_or_default().status);
            }
            let mut running = guard.take().expect("checked Some above");
            let in_flight = running.in_flight;
            if let Err(e) = running.kill().await {
                tracing::warn!(%agent, error = %e, "killing an interrupted agent's process failed");
            }
            in_flight
        };

        {
            let conn = self.db.lock().unwrap();
            if let Some(mid) = in_flight {
                messages::mark_error(&conn, mid, INTERRUPTED_BY_USER).ok();
                publish_message(&self.events, &conn, mid);
            }
            // Same reasoning as `stop`/`park`: a token outliving its process is a credential
            // with no owner. `deliver`, below, mints a fresh one for the resumed child.
            let _ = crate::db::tokens::revoke(&conn, agent);
        }
        self.set_status(agent, AgentStatus::Parked, None);
        tracing::info!(%agent, "interrupted at the caller's request");

        self.deliver(agent).await?;
        let conn = self.db.lock().unwrap();
        Ok(board::agent_state(&conn, agent).unwrap_or_default().status)
    }

    /// Stop every agent before the engine exits, without losing a turn or running one twice.
    ///
    /// Nothing starts and no new message is written once this begins. Turns already running get up
    /// to [`SHUTDOWN_DRAIN`] to finish, and are consumed as usual. A turn still running after that is
    /// consumed with [`INTERRUPTED_BY_SHUTDOWN`] rather than requeued: a turn killed mid-flight may
    /// already have committed or pushed, and running it again would do that twice.
    ///
    /// Then SIGTERM to every agent's process group at once, and SIGKILL for whatever is left after
    /// the grace period. Each agent is left `parked`, not `stopped`: its session is kept, and the
    /// first message after a restart resumes it.
    pub async fn shutdown(&self) {
        self.closing.store(true, Ordering::SeqCst);
        let drain = std::time::Duration::from_millis(self.shutdown_drain_ms.load(Ordering::SeqCst));
        let drain_until = tokio::time::Instant::now() + drain;
        while self.turns_in_flight().await > 0 && tokio::time::Instant::now() < drain_until {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }

        let mut stopping = Vec::new();
        for (agent, slot) in self.all_slots().await {
            if let Some(running) = slot.lock().await.take() {
                running.signal_group(libc::SIGTERM);
                stopping.push((agent, running));
            }
        }
        {
            let conn = self.db.lock().unwrap();
            for mid in stopping.iter().filter_map(|(_, running)| running.in_flight) {
                messages::mark_error(&conn, mid, INTERRUPTED_BY_SHUTDOWN).ok();
                publish_message(&self.events, &conn, mid);
            }
        }

        let deadline = tokio::time::Instant::now() + SHUTDOWN_GRACE;
        for (agent, mut running) in stopping {
            let _ = tokio::time::timeout_at(deadline, running.child.wait()).await;
            if let Err(e) = running.kill().await {
                tracing::warn!(%agent, error = %e, "killing an agent at shutdown failed");
            }
            {
                let conn = self.db.lock().unwrap();
                let _ = crate::db::tokens::revoke(&conn, agent);
            }
            self.set_status(agent, AgentStatus::Parked, None);
        }
    }

    async fn all_slots(&self) -> Vec<(Uuid, AgentSlot)> {
        self.agents
            .lock()
            .await
            .iter()
            .map(|(id, slot)| (*id, slot.clone()))
            .collect()
    }

    async fn turns_in_flight(&self) -> usize {
        let mut n = 0;
        for (_, slot) in self.all_slots().await {
            if slot
                .lock()
                .await
                .as_ref()
                .is_some_and(|r| r.in_flight.is_some())
            {
                n += 1;
            }
        }
        n
    }

    /// Deliver the next queued message if the agent is idle.
    ///
    /// The ONLY path that writes to a child's stdin. Strictly one message per
    /// turn: while `in_flight` is set nothing further is written, so the
    /// operator's chat and inbound agent traffic can never interleave.
    pub async fn pump_queue(self: &Arc<Self>, agent: Uuid) -> Result<()> {
        let slot = self.slot(agent).await;
        let mut guard = slot.lock().await;
        let Some(running) = guard.as_mut() else {
            return Ok(()); // stopped or parked: the queue drains on next start
        };
        if running.in_flight.is_some() {
            return Ok(()); // mid-turn
        }
        if self.closing.load(Ordering::SeqCst) {
            return Ok(()); // shutting down: what is queued waits for the next start
        }
        // Credentials failed: the queue waits for a person, and is not spent a
        // turn at a time discovering the same dead token.
        if current_status(&self.db, agent) == AgentStatus::NeedsAuth {
            return Ok(());
        }

        let next = {
            let conn = self.db.lock().unwrap();
            messages::next_for_delivery(&conn, agent, running.consecutive_user)?
        };
        let Some(msg) = next else { return Ok(()) };

        // Encoding is the one step that reads a body we did not write. An em
        // dash in a stored message once panicked the escaper here, and because
        // the message is replayed at every start, that took a whole board down
        // through repeated reboots (ADVERSARY 035). The escaper is fixed; this
        // makes the CLASS impossible: a body that cannot be encoded is set
        // aside with its reason, and the agent goes on to the next message.
        let encoded = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.harness.encode_turn(&msg.envelope())
        }));
        let line = match encoded {
            Ok(line) => line,
            Err(_) => {
                let reason = "the body could not be encoded for delivery and was set aside";
                tracing::error!(
                    message_id = %msg.id,
                    agent = %agent,
                    "quarantined a message whose body panicked the encoder"
                );
                let notify_to = {
                    let conn = self.db.lock().unwrap();
                    messages::quarantine(&conn, msg.id, reason).ok();
                    // A waiting sender learns of it now, not at its timeout.
                    publish_message(&self.events, &conn, msg.id);
                    notify_sender(&conn, &self.events, msg.id)
                };
                drop(guard);
                if let Some(sender) = notify_to {
                    self.deliver_soon(sender);
                }
                return Ok(());
            }
        };
        if let Err(e) = running.stdin.write_all(line.as_bytes()).await {
            // Never truncate and never drop: the message stays queued with a
            // visible reason (§3c#11).
            let conn = self.db.lock().unwrap();
            messages::set_last_error(&conn, msg.id, &format!("stdin write failed: {e}")).ok();
            return Ok(());
        }
        let _ = running.stdin.flush().await;

        running.consecutive_user = match msg.from {
            wheel_core::MessageSender::User => running.consecutive_user + 1,
            _ => 0,
        };
        running.in_flight = Some(msg.id);

        {
            let bus = &self.events;
            let conn = self.db.lock().unwrap();
            messages::advance(&conn, msg.id, MessageState::Delivered).ok();
            publish_message(bus, &conn, msg.id);
            // The exact bytes written, for the transcript log stream.
            log_line_bus(bus, &conn, agent, "transcript", line.trim_end());
        }
        self.set_status(agent, AgentStatus::Running, None);
        Ok(())
    }

    fn pump_stdout(
        self: Arc<Self>,
        agent: Uuid,
        stdout: tokio::process::ChildStdout,
        slot: Arc<AsyncMutex<Option<Running>>>,
        stderr_done: tokio::task::JoinHandle<String>,
        run_id: Uuid,
        secrets: Vec<String>,
    ) {
        let db = self.db.clone();
        let harness = self.harness.clone();
        let bus = self.events.clone();
        let ephemeral = {
            let conn = db.lock().unwrap();
            board::get(&conn, agent)
                .ok()
                .flatten()
                .and_then(|n| n.config.as_agent().map(|a| a.ephemeral_context))
                .unwrap_or(false)
        };
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            // The real CLI reports "Not logged in" on stdout, so the reaper
            // needs this text to classify. Bounded: a chatty child must not be
            // able to grow this without limit.
            let mut tail = StartupTail::default();
            // Whether a session ever started. A child that initialised and
            // later exited did not FAIL to start, whatever it printed on the
            // way — that distinction is what stops a normal shutdown after a
            // noisy session being reported as an error.
            let mut initialised = false;
            while let Ok(Some(line)) = lines.next_line().await {
                // A child that prints its own environment must not put a
                // secret into the log or the transcript. Accidental-echo
                // protection only: an agent that can read a value can also
                // transform it past this.
                let line = crate::vault::redact(&line, &secrets);
                tail.push(line.clone());
                let event = harness.parse_line(&line);
                match event {
                    HarnessEvent::Init { session_id } => {
                        initialised = true;
                        {
                            let mut g = slot.lock().await;
                            if let Some(r) = g.as_mut() {
                                r.session_id = Some(session_id.clone());
                            }
                        }
                        {
                            let conn = db.lock().unwrap();
                            set_session(&conn, agent, &session_id);
                            set_status_db(&conn, agent, AgentStatus::Idle, None);
                        }
                        // The child can be written to now. Anything enqueued
                        // while it was coming up has no other trigger: the
                        // queue is pumped when something enqueues and when a
                        // turn ends, and starting up is neither -- so a message
                        // that arrived during startup waited for a turn that
                        // could not begin, because beginning it was the thing
                        // being waited for. Both guards above are released
                        // first: `pump_queue` takes the same slot lock.
                        let _ = self.pump_queue(agent).await;
                    }
                    HarnessEvent::Text { session_id, text } => {
                        let g = slot.lock().await;
                        let known = g.as_ref().and_then(|r| r.session_id.clone());
                        drop(g);
                        if !session_matches(known.as_deref(), session_id.as_deref()) {
                            continue;
                        }
                        let conn = db.lock().unwrap();
                        log_line_bus(&bus, &conn, agent, "stdout", &text);
                    }
                    HarnessEvent::Result {
                        session_id,
                        is_error,
                        text,
                        turns,
                        cost_usd,
                    } => {
                        let mut g = slot.lock().await;
                        // F008: an agent controls its own stdout. A `result`
                        // whose session does not match the one we started is a
                        // forgery or a stale event, and must not end a turn.
                        let known = g.as_ref().and_then(|r| r.session_id.clone());
                        if !session_matches(known.as_deref(), session_id.as_deref()) {
                            let conn = db.lock().unwrap();
                            log_line(
                                &conn,
                                agent,
                                "engine",
                                &format!(
                                    "ignored a result event with a mismatched session_id: {session_id:?}"
                                ),
                            );
                            continue;
                        }
                        let finished = g.as_mut().and_then(|r| r.in_flight.take());
                        // A limit belongs to the turn it arrived in, never a later one.
                        let limit_seen = g.as_mut().and_then(|r| r.limit_seen.take());
                        let on_fallback = g.as_ref().is_some_and(|r| r.on_fallback);
                        // Deltas, not totals: see `Running::counted_turns`.
                        let spend = g.as_mut().map(|r| {
                            let (dt, du) =
                                spend_delta(turns, cost_usd, r.counted_turns, r.counted_usd);
                            r.counted_turns += dt;
                            r.counted_usd += du;
                            (dt, du)
                        });
                        let gen = g.as_ref().and_then(|r| r.credential_gen);
                        drop(g);

                        let over_budget = if let Some((dt, du)) = spend {
                            let conn = db.lock().unwrap();
                            if let Err(e) = board::add_spend(&conn, agent, dt, du) {
                                tracing::warn!(agent = %agent, error = %e, "could not record spend");
                            }
                            board::budget_exceeded(&conn, agent).unwrap_or(None)
                        } else {
                            None
                        };

                        // A harness error is not automatically the MESSAGE's
                        // fault. "Not logged in" arrives as a perfectly normal
                        // `result` with is_error, and consuming the message on
                        // that basis loses the operator's work to a setup
                        // problem they are about to fix. Environmental
                        // failures -- no credentials, a closed usage window --
                        // requeue; genuine task errors are consumed, because
                        // poison must not loop.
                        let needs_auth = is_error
                            && matches!(
                                harness.classify_startup_failure(
                                    None,
                                    text.as_deref().unwrap_or_default()
                                ),
                                StartupFailure::NeedsAuth
                            );
                        let end = quota::classify_turn_end(
                            is_error,
                            text.as_deref(),
                            limit_seen.as_ref(),
                            needs_auth,
                        );

                        if let quota::TurnEnd::Limited(signal) = &end {
                            self.on_limit(agent, finished, signal.clone(), on_fallback, over_budget)
                                .await;
                            continue;
                        }

                        // Settled BEFORE the budget check. A turn that ran is
                        // consumed whether or not it was the last one the
                        // budget allows; stopping first stranded it delivered.
                        let notify_to = {
                            // Scoped so the sqlite guard cannot be held across
                            // the awaits below: a rusqlite Connection is not
                            // Send, and holding its guard would make this task
                            // unspawnable.
                            let conn = db.lock().unwrap();
                            let notify_to = finished.and_then(|mid| {
                                settle_turn(&conn, &bus, mid, &end, text.as_deref())
                            });
                            if over_budget.is_none() {
                                let (status, detail) = match &end {
                                    quota::TurnEnd::NeedsAuth => (
                                        AgentStatus::NeedsAuth,
                                        Some("the harness has no usable credentials".to_string()),
                                    ),
                                    quota::TurnEnd::TaskError => {
                                        (AgentStatus::Error, Some(text.clone().unwrap_or_default()))
                                    }
                                    _ => (AgentStatus::Idle, None),
                                };
                                set_status_db(&conn, agent, status, detail);
                                publish_state(&bus, &conn, agent);
                            }
                            notify_to
                        };
                        if let Some(sender) = notify_to {
                            let _ = self.deliver(sender).await;
                        }

                        // §3e: the ceiling is enforced here because here is
                        // where the total changes. Stop first — `stop` writes
                        // `stopped` unconditionally — then record WHY, which
                        // the exit cleanup preserves because
                        // `budget_exhausted` counts as already diagnosed.
                        if let Some(reason) = over_budget {
                            tracing::warn!(agent = %agent, %reason, "agent stopped: budget reached");
                            let _ = self.stop(agent).await;
                            self.set_status(agent, AgentStatus::BudgetExhausted, Some(reason));
                            continue;
                        }

                        if end == quota::TurnEnd::NeedsAuth {
                            // Nothing more can run until credentials exist, and
                            // draining the queue into the same failure would
                            // requeue every message in turn for no reason --
                            // unless the vault already holds a newer login, or
                            // this one merely ran out and can be renewed once.
                            if !self.recover_from_auth_failure(agent, gen).await {
                                // Nothing to run on: hold the process's place
                                // rather than spending a turn per queued
                                // message to be told the same thing again.
                                self.park_needs_auth(agent).await;
                            }
                            continue;
                        }

                        // The turn is over. Either the context is discarded
                        // and rebuilt first, or the next queued message may be
                        // written now. This is the only place delivery
                        // resumes: one message per turn, never mid-turn.
                        if ephemeral {
                            if let Err(e) = self.clear_context(agent).await {
                                tracing::warn!(agent = %agent, error = %e, "ephemeral restart failed");
                            }
                        } else if !is_error && self.credential_stale(gen.as_ref()) {
                            // Between turns is the one moment a child can be
                            // swapped without losing work: the vault's login
                            // was renewed while this turn ran, so the next
                            // message goes to a child holding the new token.
                            self.recycle(agent).await;
                        } else {
                            let _ = self.pump_queue(agent).await;
                            // Nothing left to do: start counting down. If the
                            // pump delivered something, the timer's re-check
                            // finds the agent busy and does nothing.
                            self.arm_park_timer(agent);
                        }
                    }
                    HarnessEvent::RateLimit {
                        session_id,
                        status,
                        window,
                        utilization,
                        resets_at,
                    } => {
                        // The operator pays for this window and is the person
                        // who most needs to know it is closing. It used to be
                        // logged as anonymous text.
                        let pct = utilization.map(|u| u * 100.0).unwrap_or(f64::NAN);
                        let line = format!(
                            "rate limit ({}): {:.0}% of the {} window used{}",
                            status,
                            pct,
                            window.as_deref().unwrap_or("current"),
                            resets_at
                                .map(|t| format!(", resets at unix {t}"))
                                .unwrap_or_default()
                        );
                        {
                            let mut g = slot.lock().await;
                            // F008, as for `result`: an event we cannot bind
                            // to the session we started must not park the
                            // agent or move it onto the fallback credential.
                            let known = g.as_ref().and_then(|r| r.session_id.clone());
                            if !session_matches(known.as_deref(), session_id.as_deref()) {
                                drop(g);
                                let conn = db.lock().unwrap();
                                log_line(
                                    &conn,
                                    agent,
                                    "engine",
                                    &format!(
                                        "ignored a rate_limit_event with a mismatched session_id: {session_id:?}"
                                    ),
                                );
                                continue;
                            }
                            if status == "rejected" {
                                if let Some(r) = g.as_mut() {
                                    r.limit_seen = Some(quota::LimitSignal {
                                        resets_at,
                                        window: window.clone(),
                                    });
                                }
                            }
                        }
                        if status != "allowed" {
                            tracing::warn!(agent = %agent, %line, "harness reported a rate limit");
                        }
                        let conn = db.lock().unwrap();
                        board::set_quota(
                            &conn,
                            agent,
                            &wheel_core::QuotaWindow {
                                status,
                                window,
                                utilization,
                                resets_at: resets_at.and_then(quota::timestamp),
                                observed_at: wheel_core::Timestamp::now(),
                            },
                        );
                        log_line_bus(&bus, &conn, agent, "engine", &line);
                    }
                    HarnessEvent::Unknown { raw } => {
                        if raw.is_empty() {
                            continue;
                        }
                        let conn = db.lock().unwrap();
                        log_line_bus(&bus, &conn, agent, "stdout", &raw);
                    }
                }
            }

            // stdout closed: the child is gone. Reap it.
            self.reap(
                agent,
                slot,
                stderr_done,
                run_id,
                tail.into_string(),
                initialised,
            )
            .await;
        });
    }

    /// Settle a child that has exited: clear its slot, reap the process, put
    /// anything in flight back on the queue, and record why it went away.
    ///
    /// Liveness comes from the supervisor that owns the process (§3c#15), so
    /// this is the ONLY place a dead child is recognised — and it must run,
    /// because a slot left occupied makes every later `start` a silent no-op
    /// and lets `pump_queue` write into a stdin nothing is reading.
    async fn reap(
        &self,
        agent: Uuid,
        slot: Arc<AsyncMutex<Option<Running>>>,
        stderr_done: tokio::task::JoinHandle<String>,
        run_id: Uuid,
        stdout_tail: String,
        initialised: bool,
    ) {
        // Wait for stderr so the classification below sees the whole message.
        // Without this the two tasks race and the reason is a coin flip.
        let captured = stderr_done.await.unwrap_or_default();

        let mut guard = slot.lock().await;
        if guard.as_ref().map(|r| r.run_id) != Some(run_id) {
            // Either `stop()` already took the slot and set the status, or a
            // replacement child is now living in it. Settling either one would
            // report this dead process's fate as the live one's.
            return;
        }
        let Some(mut running) = guard.take() else {
            return;
        };
        let _ = running.child.wait().await;
        let in_flight = running.in_flight;
        drop(guard);

        // BOTH streams: the real `claude` CLI announces "Not logged in ·
        // Please run /login" on stdout and exits without a `result`, so a
        // stderr-only classification calls the commonest failure an operator
        // will ever see a misconfiguration and eats the queued message.
        let output = format!("{}\n{}", captured, stdout_tail);

        let settled = {
            let conn = self.db.lock().unwrap();
            board::agent_state(&conn, agent).unwrap_or_default().status
        };
        // Something may already have diagnosed this run — a `result` carrying
        // an auth failure, say. The exit that follows is a consequence of that
        // diagnosis, not a fresh one, so the cleanup below still happens but
        // the status is left alone: "stopped" would erase the only status
        // telling the operator what to do.
        let already_diagnosed = matches!(
            settled,
            AgentStatus::NeedsAuth | AgentStatus::Error | AgentStatus::BudgetExhausted
        );

        let (status, detail) = if initialised {
            // It started, so it did not fail to START. Whatever it printed
            // during a working session is not a diagnosis of its exit, and a
            // clean shutdown after a chatty session must not read as an error.
            (AgentStatus::Stopped, None)
        } else {
            match self.harness.classify_startup_failure(None, &output) {
                // Environmental, not poison: the queued message stays queued
                // and is delivered on the next start once the operator
                // authenticates. Marking it error would consume and lose it.
                StartupFailure::NeedsAuth => (
                    AgentStatus::NeedsAuth,
                    Some("the harness has no usable credentials".to_string()),
                ),
                StartupFailure::Misconfigured(why) if !output.trim().is_empty() => {
                    (AgentStatus::Error, Some(why))
                }
                // Exited with nothing to say at all.
                StartupFailure::Misconfigured(_) => (AgentStatus::Stopped, None),
            }
        };

        {
            let conn = self.db.lock().unwrap();
            // Anything written to the dying child never ran a turn, so ordinarily it goes back on
            // the queue rather than being lost as though it had been handled. But if the engine is
            // shutting down, this exit may not be the supervisor's own doing — a cgroup-wide kill
            // (review round 2, finding 1: `KillMode=control-group` sends SIGTERM to the agent
            // process at the same instant as to wheeld, well before `shutdown`'s own drain and
            // signal sequence reaches it) — and a turn killed mid-flight may already have taken an
            // effect the process never reported. Requeuing it there would run that turn again,
            // which is the replay `Supervisor::shutdown`'s own drain-timeout branch already refuses.
            if self.closing.load(Ordering::SeqCst) {
                let n = messages::consume_all_delivered_as_interrupted(
                    &conn,
                    agent,
                    INTERRUPTED_BY_SHUTDOWN,
                )
                .unwrap_or(0);
                if n > 0 {
                    tracing::info!(agent = %agent, interrupted = n, "consumed in-flight message(s) as interrupted by shutdown rather than replaying them");
                }
                if let Some(mid) = in_flight {
                    publish_message(&self.events, &conn, mid);
                }
            } else {
                let n = messages::requeue_all_undelivered(
                    &conn,
                    agent,
                    "the harness exited before this message could be processed",
                )
                .unwrap_or(0);
                if n > 0 {
                    tracing::info!(agent = %agent, requeued = n, in_flight = ?in_flight, "returned in-flight messages to the queue");
                }
            }
            // A token outliving its process is a credential with no owner.
            let _ = crate::db::tokens::revoke(&conn, agent);

            // A start that passed `--resume` and never reached `init` is the
            // signature of an unusable session. Nothing else cleared it, so
            // every later start re-passed the same dead id and failed the same
            // way — an agent that could never recover without a manual clear.
            //
            // Parking makes this matter: resume used to be rare and is now the
            // normal path back from idle.
            //
            // Dropping a session we might have kept costs the agent its context
            // once; keeping a dead one costs it every start from now on. The
            // recoverable failure is the better one.
            if !initialised {
                let had_session = board::agent_state(&conn, agent)
                    .unwrap_or_default()
                    .session_id
                    .is_some();
                if had_session {
                    clear_session(&conn, agent);
                    tracing::warn!(
                        %agent,
                        "a resumed session never initialised; cleared it so the next start is fresh"
                    );
                }
            }
            if !already_diagnosed {
                set_status_db(&conn, agent, status, detail);
            }
            publish_state(&self.events, &conn, agent);
        }
    }

    /// Log the child's stderr, and hand the captured text to [`Supervisor::reap`].
    ///
    /// Deliberately does NOT decide the agent's status: when this task and the
    /// stdout task both wrote status, the one that lost the race decided it.
    fn pump_stderr(
        &self,
        agent: Uuid,
        stderr: tokio::process::ChildStderr,
        secrets: Vec<String>,
    ) -> tokio::task::JoinHandle<String> {
        let db = self.db.clone();
        let bus = self.events.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            let mut captured = String::new();
            while let Ok(Some(line)) = lines.next_line().await {
                let line = crate::vault::redact(&line, &secrets);
                captured.push_str(&line);
                captured.push('\n');
                let conn = db.lock().unwrap();
                // stderr is log material, never parsed as JSON.
                log_line_bus(&bus, &conn, agent, "stderr", &line);
            }
            captured
        })
    }

    /// Discard an agent's context and rebuild it: a NEW harness session with
    /// the system prompt and every wired ctx node re-injected, then the queue
    /// drains again.
    ///
    /// This is `ephemeral_context` after a turn and `wheel ctx clear` on
    /// demand; both want the same thing, so they are the same code path. The
    /// session id is cleared BEFORE the restart, or the new child would
    /// `--resume` the context we are throwing away.
    pub async fn clear_context(self: &Arc<Self>, agent: Uuid) -> Result<AgentStatus> {
        {
            let slot = self.slot(agent).await;
            let mut guard = slot.lock().await;
            if let Some(mut r) = guard.take() {
                if let Err(e) = r.kill().await {
                    tracing::warn!(%agent, error = %e, "killing a cleared agent's process failed");
                }
            }
        }
        {
            let conn = self.db.lock().unwrap();
            clear_session(&conn, agent);
        }
        // PARKED, not a fresh start. This used to spawn a replacement child
        // immediately, and for an ephemeral agent that restart happens after
        // EVERY turn -- into a queue the turn just emptied.
        //
        // Nothing was then written to that child's stdin, and `claude` emits
        // its `system/init` only when it processes a turn, so no init arrived;
        // the only Starting -> Idle transition is the Init arm, so the agent
        // sat in `starting` until the next message. That is where the
        // operator's own agent LIVED between turns -- the only ephemeral one on
        // the board, and the only one stuck (PM's discriminator, ADVERSARY 041).
        //
        // The cost was the expensive half: idle parking keys on `idle` (§3c#14),
        // so an agent that never reached it held a live harness process 24/7
        // doing nothing. That is precisely the "one live process forever" this
        // project exists to avoid.
        //
        // Parking deletes the special case instead of adding one: an ephemeral
        // agent now uses the same parked -> resume path as every other agent,
        // and the session is already cleared, so the resume starts fresh rather
        // than reviving what was discarded.
        self.set_status(agent, AgentStatus::Parked, None);

        // A turn may have queued work behind it. `deliver` resumes a parked
        // agent that has something waiting, and does nothing when it does not
        // -- so an empty queue costs no process, and a full one does not sit
        // waiting for some LATER message to trigger it. That second half is the
        // bug this engine already had once, from the other direction.
        self.deliver(agent).await?;

        let conn = self.db.lock().unwrap();
        Ok(board::agent_state(&conn, agent).unwrap_or_default().status)
    }

    /// Bring the board up. Agents configured `run_on_startup` come up
    /// **parked**, not running (§2: `run_on_startup` starts them parked).
    ///
    /// Parked means "logically on, no process": the agent costs nothing until
    /// something addresses it, and [`Supervisor::deliver`] resumes it on the
    /// first message. Spawning every such agent at boot is what makes a board
    /// of twenty agents cost twenty idle processes, which is the bill this
    /// project exists to avoid. An agent that is never messaged therefore
    /// never spawns — that is the intended trade, not an oversight.
    pub async fn start_configured_agents(self: &Arc<Self>) {
        // The resume timers died with the last process; their resume times
        // did not, because they are in agent_state.
        self.rearm_quota_timers();
        let agents: Vec<Uuid> = {
            let conn = self.db.lock().unwrap();
            board::list(&conn)
                .unwrap_or_default()
                .into_iter()
                .filter(|n| {
                    n.config
                        .as_agent()
                        .map(|a| a.run_on_startup)
                        .unwrap_or(false)
                })
                // Parking it would erase its reset time and resume it
                // straight into the closed window.
                .filter(|n| {
                    board::agent_state(&conn, n.id).unwrap_or_default().status
                        != AgentStatus::RateLimited
                })
                .map(|n| n.id)
                .collect()
        };
        if agents.is_empty() {
            return;
        }
        {
            let conn = self.db.lock().unwrap();
            for id in &agents {
                set_status_db(&conn, *id, AgentStatus::Parked, None);
                publish_state(&self.events, &conn, *id);
            }
        }
        tracing::info!(count = agents.len(), "agents parked on startup");

        // Anything already queued from a previous run is addressed to them
        // now, which resumes exactly the agents that have work.
        for id in agents {
            let _ = self.deliver(id).await;
        }
    }

    /// Which harness binary this supervisor drives. The login flow must spawn
    /// the same one, or an agent could be signed in to a CLI it never runs.
    pub fn harness_program(&self) -> &str {
        self.harness.program()
    }

    /// Deliver to an agent, resuming it first if it is parked.
    ///
    /// Every enqueue path goes through here rather than calling `pump_queue`
    /// directly: a parked agent has no process to write to, and without the
    /// resume its messages would sit in the queue looking delivered-any-moment
    /// forever.
    pub async fn deliver(self: &Arc<Self>, agent: Uuid) -> Result<()> {
        let (status, waiting) = {
            let conn = self.db.lock().unwrap();
            let status = board::agent_state(&conn, agent).unwrap_or_default().status;
            // Deliberately not `unwrap_or(false)`: a failed lookup would read
            // as "nothing is waiting" and strand the queue behind a parked
            // agent, which is indistinguishable from an idle board.
            let waiting = messages::has_queued(&conn, agent)?;
            (status, waiting)
        };
        if waiting && matches!(status, AgentStatus::Parked) {
            self.start(agent).await?;
        }
        self.pump_queue(agent).await
    }

    fn set_status(&self, agent: Uuid, status: AgentStatus, err: Option<String>) {
        let conn = self.db.lock().unwrap();
        set_status_db(&conn, agent, status, err);
        publish_state(&self.events, &conn, agent);
    }

    /// `deliver` on its own task, for a caller already inside one agent's
    /// delivery. Awaiting it there would nest one agent's slot lock inside
    /// another's, and an async fn reaching itself cannot be proven Send.
    fn deliver_soon(self: &Arc<Self>, agent: Uuid) {
        let me = Arc::clone(self);
        tokio::spawn(async move {
            let _ = me.deliver(agent).await;
        });
    }

    /// Which account this spawn runs as: the usable fallback vault, if any,
    /// and whether to run on it now.
    fn credential_plan(
        &self,
        agent: Uuid,
        cfg: &wheel_core::AgentConfig,
        fallback_until: Option<wheel_core::Timestamp>,
    ) -> (Option<(Uuid, String)>, bool) {
        let conn = self.db.lock().unwrap();
        let fallback = crate::vault::fallback_vault(&conn, agent).ok().flatten();
        if fallback.is_none() && cfg.fallback_vault.is_some() {
            log_line_bus(
                &self.events,
                &conn,
                agent,
                "engine",
                "fallback_vault is set, but this agent no longer has a read wire to it, so it \
                 will not be used",
            );
        }
        let until = fallback_until.filter(|t| *t > wheel_core::Timestamp::now());
        let use_fallback = match (&fallback, until) {
            (Some((_, vault)), Some(until)) => {
                log_line_bus(
                    &self.events,
                    &conn,
                    agent,
                    "engine",
                    &format!("starting on the fallback credential from vault {vault} until {until}"),
                );
                true
            }
            _ => false,
        };
        (fallback, use_fallback)
    }

    /// A turn ended on a closed usage window (parity proposal §1.2): requeue
    /// what it was carrying, stop the child, then either restart on the
    /// fallback credential or park until the window reopens.
    async fn on_limit(
        self: &Arc<Self>,
        agent: Uuid,
        finished: Option<Uuid>,
        signal: quota::LimitSignal,
        on_fallback: bool,
        over_budget: Option<String>,
    ) {
        let now = time::OffsetDateTime::now_utc();
        let (strikes, notify_to) = {
            let conn = self.db.lock().unwrap();
            requeue_limited(&conn, &self.events, finished)
        };
        if let Some(sender) = notify_to {
            let _ = self.deliver(sender).await;
        }
        self.halt_child(agent).await;

        if let Some(reason) = over_budget {
            self.set_status(agent, AgentStatus::BudgetExhausted, Some(reason));
            return;
        }

        let resume = quota::resume_at(now, signal.resets_at, strikes, self.jitter());
        let (fallback, fallback_until) = {
            let conn = self.db.lock().unwrap();
            (
                crate::vault::fallback_vault(&conn, agent).ok().flatten(),
                board::agent_state(&conn, agent)
                    .unwrap_or_default()
                    .fallback_until,
            )
        };

        // Once per closed window: a limit ON the fallback parks rather than
        // bouncing back, so two limited accounts cannot become a loop.
        if let (false, Some((_, vault))) = (on_fallback, &fallback) {
            {
                let conn = self.db.lock().unwrap();
                board::set_fallback_until(&conn, agent, Some(resume.into()));
                log_line_bus(
                    &self.events,
                    &conn,
                    agent,
                    "engine",
                    &format!(
                        "the usage window closed on this agent's own credential; restarting on \
                         the fallback credential from vault {vault} until {}",
                        wheel_core::Timestamp::from(resume)
                    ),
                );
            }
            self.set_status(agent, AgentStatus::Parked, None);
            let _ = self.deliver(agent).await;
            return;
        }

        let resume = match (on_fallback, fallback_until) {
            (true, Some(until)) => resume.min(until.into_inner()),
            _ => resume,
        };
        let reason = format!(
            "the {} usage window is closed; the in-flight message was requeued and delivery \
             resumes at {}",
            signal.window.as_deref().unwrap_or("harness"),
            wheel_core::Timestamp::from(resume)
        );
        {
            let conn = self.db.lock().unwrap();
            board::set_rate_limited(
                &conn,
                agent,
                signal.resets_at.and_then(quota::timestamp),
                resume.into(),
                &reason,
            );
            publish_state(&self.events, &conn, agent);
            log_line_bus(&self.events, &conn, agent, "engine", &reason);
        }
        tracing::warn!(%agent, %reason, "agent parked on a closed usage window");
        self.arm_resume_timer(agent, resume);
    }

    /// Kill an agent's child and revoke its token, leaving the status to the
    /// caller. The dying child's reaper then finds the slot empty and settles
    /// nothing, exactly as after `stop`.
    async fn halt_child(&self, agent: Uuid) {
        let slot = self.slot(agent).await;
        let taken = slot.lock().await.take();
        if let Some(mut r) = taken {
            if let Err(e) = r.child.kill().await {
                tracing::warn!(%agent, error = %e, "killing a rate-limited agent's process failed; kill_on_drop is the backstop");
            }
        }
        let conn = self.db.lock().unwrap();
        let _ = crate::db::tokens::revoke(&conn, agent);
    }

    /// One sleep, then one check. No polling (§2): a limited agent costs a
    /// sleeping task and nothing else.
    fn arm_resume_timer(self: &Arc<Self>, agent: Uuid, at: time::OffsetDateTime) {
        let me = Arc::clone(self);
        tokio::spawn(async move {
            let wait = (at - time::OffsetDateTime::now_utc()).max(time::Duration::ZERO);
            tokio::time::sleep(wait.unsigned_abs()).await;
            me.resume_after_limit(agent).await;
        });
    }

    /// The resume timer firing. Inert unless the agent is still parked on a
    /// window that is now due: an operator's start or stop, or a newer limit
    /// with a later resume, win without any cancellation bookkeeping.
    async fn resume_after_limit(self: &Arc<Self>, agent: Uuid) {
        let due = {
            let conn = self.db.lock().unwrap();
            let state = board::agent_state(&conn, agent).unwrap_or_default();
            let slack = time::OffsetDateTime::now_utc() + time::Duration::seconds(1);
            state.status == AgentStatus::RateLimited
                && state.resume_at.is_none_or(|t| t.into_inner() <= slack)
        };
        if !due {
            return;
        }
        // Parked, so `deliver` resumes it only if work is waiting: a timer
        // firing on an empty queue costs no process.
        self.set_status(agent, AgentStatus::Parked, None);
        let _ = self.deliver(agent).await;
    }

    fn rearm_quota_timers(self: &Arc<Self>) {
        let limited = {
            let conn = self.db.lock().unwrap();
            board::rate_limited_agents(&conn).unwrap_or_default()
        };
        for (agent, at) in limited {
            let at = at.map_or_else(time::OffsetDateTime::now_utc, |t| t.into_inner());
            self.arm_resume_timer(agent, at);
        }
    }

    fn jitter(&self) -> u64 {
        use rand::Rng;
        rand::thread_rng().gen_range(self.limit_jitter.clone())
    }

    /// Record `waiter` as blocked on `target` until the guard drops, or refuse
    /// because that would close a cycle of waits (parity proposal §2.5).
    pub fn begin_await(
        &self,
        waiter: Uuid,
        target: Uuid,
    ) -> Result<awaits::AwaitGuard, awaits::AwaitRefused> {
        awaits::begin(&self.awaits, waiter, target)
    }

    /// Wait until message `id` settles or `secs` pass, then say where it
    /// stands. No lock is held while waiting: delivery, reaping and every
    /// other agent carry on underneath.
    ///
    /// Subscribed BEFORE the first read, so a turn finishing between the two
    /// is still seen. A lagged subscriber re-reads rather than trusting that
    /// it saw every event.
    pub async fn await_settlement(&self, id: Uuid, secs: u64) -> Option<messages::Settlement> {
        use tokio::sync::broadcast::error::RecvError;
        let mut rx = self.events.subscribe();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(secs);
        loop {
            let now = self.settlement_of(id);
            if now.as_ref().is_none_or(messages::Settlement::is_terminal) {
                return now;
            }
            loop {
                match tokio::time::timeout_at(deadline, rx.recv()).await {
                    Err(_) | Ok(Err(RecvError::Closed)) => return self.settlement_of(id),
                    Ok(Ok(wheel_core::Event::Message { message })) if message.id == id => break,
                    Ok(Err(RecvError::Lagged(_))) => break,
                    Ok(Ok(_)) => {}
                }
            }
        }
    }

    fn settlement_of(&self, id: Uuid) -> Option<messages::Settlement> {
        let conn = self.db.lock().unwrap();
        messages::settlement(&conn, id).ok().flatten()
    }
}

/// Settle the message a finished turn consumed. Returns the agent to deliver a
/// completion notification to, when one was asked for and enqueued.
fn settle_turn(
    conn: &rusqlite::Connection,
    bus: &crate::events::Bus,
    mid: Uuid,
    end: &quota::TurnEnd,
    text: Option<&str>,
) -> Option<Uuid> {
    match end {
        quota::TurnEnd::NeedsAuth => {
            messages::requeue_undelivered(
                conn,
                mid,
                text.unwrap_or("the harness could not run this turn"),
            )
            .ok();
            publish_message(bus, conn, mid);
            None
        }
        quota::TurnEnd::TaskError => {
            messages::mark_error(conn, mid, text.unwrap_or("harness reported an error")).ok();
            publish_message(bus, conn, mid);
            notify_sender(conn, bus, mid)
        }
        quota::TurnEnd::Completed => {
            messages::complete(conn, mid, text).ok();
            publish_message(bus, conn, mid);
            notify_sender(conn, bus, mid)
        }
        quota::TurnEnd::Limited(_) => None,
    }
}

/// Requeue a limited turn's message within the cap, or consume it with an
/// error past the cap. Returns the strike count for the backoff and, past the
/// cap, the sender to notify.
fn requeue_limited(
    conn: &rusqlite::Connection,
    bus: &crate::events::Bus,
    finished: Option<Uuid>,
) -> (u32, Option<Uuid>) {
    let Some(mid) = finished else {
        return (1, None);
    };
    let reason = "rate limited: the turn did not complete, so the message was requeued until the \
                  usage window reopens";
    match messages::requeue_for_limit(conn, mid, reason, quota::MAX_LIMIT_REQUEUES) {
        Ok(messages::LimitRequeue::Requeued(n)) => {
            publish_message(bus, conn, mid);
            (n, None)
        }
        Ok(messages::LimitRequeue::CapReached(n)) => {
            messages::mark_error(
                conn,
                mid,
                &format!(
                    "the usage window closed on this message's turn {} times; it was consumed \
                     with an error rather than requeued again, so it cannot come back for ever",
                    n + 1
                ),
            )
            .ok();
            publish_message(bus, conn, mid);
            (n, notify_sender(conn, bus, mid))
        }
        _ => (1, None),
    }
}

/// Enqueue the one completion notification a settled message asked for.
///
/// Once only: [`messages::claim_notification`] lets exactly one caller past.
/// The detail is withheld when the sender no longer holds a send wire to the
/// recipient (046: the capability must hold at the moment of disclosure).
/// Returns the sender, who needs a `deliver`.
fn notify_sender(conn: &rusqlite::Connection, bus: &crate::events::Bus, mid: Uuid) -> Option<Uuid> {
    if !messages::claim_notification(conn, mid).unwrap_or(false) {
        return None;
    }
    let msg = messages::get(conn, mid).ok()??;
    let wheel_core::MessageSender::Node {
        id: sender,
        node_type: NodeType::Agent,
        ..
    } = msg.from
    else {
        return None;
    };
    let settled = messages::settlement(conn, mid).ok()??;
    let recipient = board::get(conn, msg.to)
        .ok()
        .flatten()
        .map(|n| n.name.to_string())
        .unwrap_or_else(|| msg.to.to_string());
    let still_wired = board::wires_from(conn, sender)
        .map(|ws| {
            ws.iter()
                .any(|w| w.to == msg.to && w.wire_type == wheel_core::WireType::Send)
        })
        .unwrap_or(false);
    let body = awaits::notification_body(mid, &recipient, &settled, still_wired);
    let note = messages::enqueue(conn, wheel_core::MessageSender::System, sender, body, Some(mid))
        .ok()?;
    bus.publish(wheel_core::Event::Message { message: note });
    Some(sender)
}

/// Turn the harness's CUMULATIVE session figures into the deltas to add to an
/// agent's running totals.
///
/// Both `num_turns` and `total_cost_usd` count the whole session so far: turn
/// two of a session reports 2, not 1. Adding the reports directly makes a
/// board's `turns` the sum of a triangular series — three turns would read as
/// six, and the money would be wrong in the same shape.
///
/// Extracted from the delivery loop because this arithmetic is the part that
/// can be quietly wrong: a shell harness can prove spend is WIRED, but proving
/// it is COUNTED needs a value, not a subprocess.
///
/// A missing count means one more turn (we know a turn completed — that is what
/// a `result` is). A figure that goes BACKWARDS contributes nothing rather than
/// a negative: a harness that resets its counter mid-session must not be able
/// to refund an agent's budget.
fn spend_delta(
    reported_turns: Option<u64>,
    reported_usd: Option<f64>,
    counted_turns: u64,
    counted_usd: f64,
) -> (u64, f64) {
    let turns = reported_turns
        .unwrap_or(counted_turns + 1)
        .saturating_sub(counted_turns);
    let usd = (reported_usd.unwrap_or(counted_usd) - counted_usd).max(0.0);
    (turns, usd)
}

/// Does an event's session id match the session this supervisor started?
///
/// F008: an agent controls its own stdout, so an event we cannot attribute to
/// the session we started is not allowed to end a turn. Before init there is
/// nothing to compare against, so events are accepted; afterwards an absent or
/// differing session id is refused.
///
/// Takes the two ids rather than the whole `Running` so it is directly
/// testable — the forged-event case is the one thing here that must not rot.
fn session_matches(known: Option<&str>, event: Option<&str>) -> bool {
    match known {
        None => true,
        Some(known) => event == Some(known),
    }
}

/// Read the agent's state back out and broadcast it, so subscribers always see
/// what the database says rather than what the caller intended to write.
fn publish_state(bus: &crate::events::Bus, conn: &rusqlite::Connection, agent: Uuid) {
    if let Ok(state) = crate::db::board::agent_state(conn, agent) {
        bus.publish(wheel_core::Event::NodeState {
            node_id: agent,
            state: wheel_core::NodeState::Agent(state),
        });
    }
}

fn set_status_db(
    conn: &rusqlite::Connection,
    agent: Uuid,
    status: AgentStatus,
    err: Option<String>,
) {
    // A credential warning outlives the next status change. It says something
    // about the login rather than about this transition, and it has to still be
    // there when the operator looks -- otherwise the one warning they get
    // before their agents stop is erased by the agent starting normally.
    let _ = conn.execute(
        "INSERT INTO agent_state (node_id,status,last_activity,last_error)
         VALUES (?1,?2,?3,?4)
         ON CONFLICT(node_id) DO UPDATE SET status=?2, last_activity=?3, last_error =
             CASE WHEN ?4 IS NOT NULL THEN ?4
                  WHEN last_error LIKE ?5 || '%' THEN last_error
                  ELSE NULL END,
             resets_at=NULL, resume_at=NULL",
        rusqlite::params![
            agent.to_string(),
            status.as_str(),
            wheel_core::Timestamp::now().to_rfc3339(),
            err,
            refresh::WARNING_PREFIX,
        ],
    );
}

/// Forget the resumable session, so the next start is a NEW context rather
/// than a `--resume` of the one being discarded.
fn current_status(db: &Mutex<rusqlite::Connection>, agent: Uuid) -> AgentStatus {
    let conn = db.lock().unwrap();
    board::agent_state(&conn, agent).unwrap_or_default().status
}

fn clear_session(conn: &rusqlite::Connection, agent: Uuid) {
    let _ = conn.execute(
        "UPDATE agent_state SET session_id = NULL WHERE node_id = ?1",
        rusqlite::params![agent.to_string()],
    );
}

fn set_session(conn: &rusqlite::Connection, agent: Uuid, session: &str) {
    let _ = conn.execute(
        "UPDATE agent_state SET session_id = ?2 WHERE node_id = ?1",
        rusqlite::params![agent.to_string(), session],
    );
}

/// Append a log line and broadcast it. Returns nothing: the broadcast is part
/// of writing a line, so no call site can persist one and forget to publish it.
fn log_line_bus(
    bus: &crate::events::Bus,
    conn: &rusqlite::Connection,
    agent: Uuid,
    stream: &str,
    text: &str,
) {
    let seq = log_line(conn, agent, stream, text);
    let Ok(stream_parsed) = serde_json::from_value(serde_json::Value::String(stream.to_string()))
    else {
        return;
    };
    bus.publish(wheel_core::Event::Log {
        line: wheel_core::LogLine {
            node_id: agent,
            seq,
            stream: stream_parsed,
            at: wheel_core::Timestamp::now(),
            text: text.to_string(),
        },
    });
}

/// Persist one log line, returning its per-agent sequence number.
fn log_line(conn: &rusqlite::Connection, agent: Uuid, stream: &str, text: &str) -> u64 {
    let seq: i64 = conn
        .query_row(
            "SELECT COALESCE(MAX(seq),0)+1 FROM logs WHERE node_id = ?1",
            rusqlite::params![agent.to_string()],
            |r| r.get(0),
        )
        .unwrap_or(1);
    let _ = conn.execute(
        "INSERT INTO logs (node_id,seq,stream,at,text) VALUES (?1,?2,?3,?4,?5)",
        rusqlite::params![
            agent.to_string(),
            seq,
            stream,
            wheel_core::Timestamp::now().to_rfc3339(),
            text
        ],
    );
    seq as u64
}

/// Write a secret to a file only its owner can read, creating it with 0600
/// from the start rather than chmod-ing afterwards — a token that is briefly
/// world-readable is a token that leaked.
fn write_secret_file(path: &std::path::Path, contents: &str) -> std::io::Result<()> {
    use std::{io::Write, os::unix::fs::OpenOptionsExt};
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(contents.as_bytes())?;
    f.flush()
}

/// Broadcast a message row after a state transition, so the UI can show
/// queued -> delivered -> consumed as it happens (§3c#4).
fn publish_message(bus: &crate::events::Bus, conn: &rusqlite::Connection, id: Uuid) {
    if let Ok(Some(m)) = messages::get(conn, id) {
        bus.publish(wheel_core::Event::Message { message: m });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A harness backed by a shell script, so tests can exercise the real
    /// spawn/exit path — including what happens after a child dies — without
    /// a model, a network, or a credential.
    struct ShimDriver {
        program: String,
    }

    impl Harness for ShimDriver {
        fn program(&self) -> &str {
            &self.program
        }
        fn argv(&self, _spec: &SpawnSpec) -> Vec<std::ffi::OsString> {
            Vec::new()
        }
        fn env(&self, _spec: &SpawnSpec) -> Vec<(String, String)> {
            Vec::new()
        }
        fn encode_turn(&self, envelope: &str) -> String {
            format!("{envelope}\n")
        }
        fn parse_line(&self, line: &str) -> HarnessEvent {
            // Same wire format as the real driver, so turn handling is
            // exercised rather than stubbed.
            ClaudeDriver.parse_line(line)
        }
        fn classify_startup_failure(&self, _code: Option<i32>, stderr: &str) -> StartupFailure {
            ClaudeDriver.classify_startup_failure(None, stderr)
        }
    }

    /// A driver whose `encode_turn` PANICS on a marked body and behaves
    /// normally otherwise (ADVERSARY 040).
    ///
    /// The `catch_unwind` belt in `pump_queue` had zero tests: `panic = "unwind"`
    /// is load-bearing in Cargo.toml and nothing proved the belt caught
    /// anything, so a refactor dropping it would have gone unnoticed until a
    /// stored body took a board down again — which is the incident it was
    /// written for.
    ///
    /// Panicking on ONE body rather than all of them is what lets the test
    /// assert the part that matters: the poison is set aside AND the agent goes
    /// on to the next message. A driver that always panicked could only show
    /// that everything stops.
    struct PoisonDriver {
        program: String,
    }

    const POISON: &str = "PANIC-ON-THIS-BODY";

    impl crate::harness::Harness for PoisonDriver {
        fn program(&self) -> &str {
            &self.program
        }
        fn argv(&self, spec: &SpawnSpec) -> Vec<std::ffi::OsString> {
            ClaudeDriver.argv(spec)
        }
        fn env(&self, spec: &SpawnSpec) -> Vec<(String, String)> {
            ClaudeDriver.env(spec)
        }
        fn encode_turn(&self, envelope: &str) -> String {
            assert!(
                !envelope.contains(POISON),
                "PoisonDriver: this body is the one that panics"
            );
            ClaudeDriver.encode_turn(envelope)
        }
        fn parse_line(&self, line: &str) -> HarnessEvent {
            ClaudeDriver.parse_line(line)
        }
        fn classify_startup_failure(&self, _code: Option<i32>, stderr: &str) -> StartupFailure {
            ClaudeDriver.classify_startup_failure(None, stderr)
        }
    }

    /// Builds a supervisor whose child is `script`, over an in-memory board
    /// holding one agent node. Returns the agent's id and its scratch dir.
    fn shim_supervisor(name: &str, script: &str) -> (Arc<Supervisor>, Uuid, std::path::PathBuf) {
        shim_supervisor_cfg(name, script, |_| {})
    }

    fn shim_supervisor_cfg(
        name: &str,
        script: &str,
        tweak: impl FnOnce(&mut wheel_core::AgentConfig),
    ) -> (Arc<Supervisor>, Uuid, std::path::PathBuf) {
        shim_supervisor_full(
            name,
            script,
            tweak,
            crate::config::DEFAULT_STARTUP_DEADLINE_SECS,
        )
    }

    /// Per-engine deadline, so a test that needs a short one does not have to
    /// mutate a process-wide variable every other test is also reading.
    /// As `shim_supervisor`, with a driver of the caller's choosing.
    fn shim_supervisor_driver(
        name: &str,
        script: &str,
        driver: impl FnOnce(String) -> Arc<dyn crate::harness::Harness> + 'static,
    ) -> (Arc<Supervisor>, Uuid, std::path::PathBuf) {
        shim_supervisor_inner(
            name,
            script,
            |_| {},
            crate::config::DEFAULT_STARTUP_DEADLINE_SECS,
            Some(Box::new(driver)),
            crate::config::HarnessAuthPolicy::default(),
        )
    }

    fn shim_supervisor_full(
        name: &str,
        script: &str,
        tweak: impl FnOnce(&mut wheel_core::AgentConfig),
        deadline_secs: u64,
    ) -> (Arc<Supervisor>, Uuid, std::path::PathBuf) {
        shim_supervisor_inner(
            name,
            script,
            tweak,
            deadline_secs,
            None,
            crate::config::HarnessAuthPolicy::default(),
        )
    }

    /// As `shim_supervisor`, on a deployment running `api-key-only`
    /// (wheel-harness-auth.md) rather than the permissive default.
    fn shim_supervisor_api_key_only(
        name: &str,
        script: &str,
    ) -> (Arc<Supervisor>, Uuid, std::path::PathBuf) {
        shim_supervisor_inner(
            name,
            script,
            |_| {},
            crate::config::DEFAULT_STARTUP_DEADLINE_SECS,
            None,
            crate::config::HarnessAuthPolicy::ApiKeyOnly,
        )
    }

    #[allow(clippy::type_complexity)]
    fn shim_supervisor_inner(
        name: &str,
        script: &str,
        tweak: impl FnOnce(&mut wheel_core::AgentConfig),
        deadline_secs: u64,
        driver: Option<Box<dyn FnOnce(String) -> Arc<dyn crate::harness::Harness> + 'static>>,
        harness_auth: crate::config::HarnessAuthPolicy,
    ) -> (Arc<Supervisor>, Uuid, std::path::PathBuf) {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!(
            "wheel-sup-{name}-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let program = dir.join("harness.sh");
        std::fs::write(&program, script).unwrap();
        std::fs::set_permissions(&program, PermissionsExt::from_mode(0o755)).unwrap();

        let conn = crate::db::open_memory().unwrap();
        let mut agent_cfg = wheel_core::AgentConfig {
            harness: wheel_core::Harness::Claude,
            system_prompt: "test".into(),
            ..Default::default()
        };
        tweak(&mut agent_cfg);
        let node = wheel_core::Node::new(
            Uuid::new_v4(),
            name.parse().unwrap(),
            wheel_core::Position::default(),
            wheel_core::NodeConfig::Agent(agent_cfg),
        );
        let id = node.id;
        board::create(&conn, &node).unwrap();

        let cfg = Arc::new(Config {
            project_id: Uuid::new_v4(),
            engine_secret: "0123456789abcdef".into(),
            vault_key: None,
            data_dir: dir.clone(),
            listen: wheel_core::ListenAddr::parse("tcp://127.0.0.1:7999").unwrap(),
            json_logs: false,
            tool_allow_hosts: Vec::new(),
            startup_deadline_secs: deadline_secs,
            harness_auth,
            script_execution_enabled: false,
        });
        let sup = Arc::new(Supervisor::with_harness(
            cfg,
            Arc::new(Mutex::new(conn)),
            Arc::new(crate::events::Bus::new()),
            match driver {
                Some(make) => make(program.display().to_string()),
                None => Arc::new(ShimDriver {
                    program: program.display().to_string(),
                }),
            },
        )
        .with_limit_jitter(0..=0));
        (sup, id, dir)
    }

    fn spend_of(sup: &Supervisor, id: Uuid) -> (u64, f64) {
        let conn = sup.db.lock().unwrap();
        let s = board::agent_state(&conn, id)
            .unwrap_or_default()
            .spend
            .unwrap_or_default();
        (s.turns, s.usd)
    }

    fn engine_log(sup: &Supervisor, id: Uuid) -> Vec<String> {
        let conn = sup.db.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT text FROM logs WHERE node_id = ?1 AND stream = 'engine' ORDER BY seq")
            .unwrap();
        let rows = stmt
            .query_map(rusqlite::params![id.to_string()], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        rows
    }

    /// A workspace that cannot be cloned leaves the agent running with a
    /// directory that is not there. Logged only to the engine's own log, that
    /// reads later as "the agent did something odd"; the reason has to sit on
    /// the agent's stream, next to the behaviour it explains.
    #[tokio::test]
    async fn a_failed_workspace_is_visible_on_the_agents_own_log() {
        let (sup, id, _dir) = shim_supervisor_cfg("wsfail", ECHO_HARNESS, |cfg| {
            cfg.workspaces = vec![wheel_core::Workspace {
                path: "r".into(),
                git: Some(wheel_core::GitSource {
                    // Nonexistent local path: fails at once, no network.
                    url: "file:///nonexistent/wheel-test-repo.git".into(),
                    git_ref: None,
                    vault_ref: None,
                }),
            }];
        });

        sup.start(id).await.expect("the agent must still start");

        let lines = engine_log(&sup, id);
        assert!(
            lines
                .iter()
                .any(|l| l.contains("workspace") && l.contains("\"r\"")),
            "the operator must be able to see WHICH workspace failed, got: {lines:?}"
        );
    }

    /// The harness field picks credentials, not the binary. Before the guard,
    /// starting a codex node spawned `claude` with codex credentials and told
    /// nobody — the operator's choice silently substituted.
    #[tokio::test]
    async fn a_codex_agent_refuses_to_start_rather_than_silently_running_claude() {
        let (sup, id, dir) = shim_supervisor_cfg("codexnode", ECHO_HARNESS, |cfg| {
            cfg.harness = wheel_core::Harness::Codex;
        });

        let status = sup
            .start(id)
            .await
            .expect("start must report, not error out");

        assert_eq!(
            status,
            AgentStatus::Error,
            "a harness this build cannot run must refuse, not start"
        );
        assert_eq!(
            runs(&dir),
            0,
            "NOTHING may be spawned for a codex node — a process here is the silent substitution itself"
        );
        let conn = sup.db.lock().unwrap();
        let last = board::agent_state(&conn, id).unwrap_or_default().last_error;
        assert!(
            last.unwrap_or_default().contains("codex"),
            "the operator must be told which harness was refused"
        );
    }

    /// (a) The agent is STARTED in its worktree, so cwd was always right; what
    /// was missing was a way to NAME it. A script that has `cd`-ed cannot
    /// recover the path from `pwd`, and there was no variable to ask.
    #[tokio::test]
    async fn a_materialised_workspace_is_named_by_wheel_workspace() {
        // No git needed: a workspace without a git source still materialises a
        // directory, and the directory is the thing under test.
        let (sup, id, dir) = shim_supervisor_cfg("wsenv", ENV_DUMP_HARNESS, |cfg| {
            cfg.workspaces = vec![wheel_core::Workspace {
                path: "wheel".into(),
                git: None,
            }];
        });
        sup.start(id).await.unwrap();
        let dumped = dir.join("child-env");
        until("the child to report its environment", || dumped.exists()).await;
        let env = std::fs::read_to_string(&dumped).unwrap();

        let ws = env
            .lines()
            .find_map(|l| l.strip_prefix("WHEEL_WORKSPACE="))
            .expect("WHEEL_WORKSPACE was not exported for an agent that HAS a workspace");
        let pwd = env
            .lines()
            .find_map(|l| l.strip_prefix("PWD="))
            .expect("no working directory");
        let real =
            |p: &str| std::fs::canonicalize(p).unwrap_or_else(|_| std::path::PathBuf::from(p));
        assert_eq!(
            real(ws),
            real(pwd),
            "WHEEL_WORKSPACE must name the directory the child is actually in"
        );
        assert!(
            ws.ends_with("/wheel"),
            "it must be the WORKTREE, not the bare object store: {ws}"
        );
    }

    /// An agent with no workspace must not get an empty or invented value: an
    /// empty WHEEL_WORKSPACE reads as "there is one, and it is nowhere".
    #[tokio::test]
    async fn an_agent_without_a_workspace_gets_no_wheel_workspace_at_all() {
        let (sup, id, dir) = shim_supervisor("nows", ENV_DUMP_HARNESS);
        sup.start(id).await.unwrap();
        let dumped = dir.join("child-env");
        until("the child to report its environment", || dumped.exists()).await;
        let env = std::fs::read_to_string(&dumped).unwrap();
        assert!(
            !env.lines().any(|l| l.starts_with("WHEEL_WORKSPACE=")),
            "WHEEL_WORKSPACE must be ABSENT, not empty, when no workspace was materialised"
        );
    }

    /// ADVERSARY, tracing the same stop-epoch lock #64 added the check in
    /// `start` for: `workspace::materialise`'s `.await` is the ONLY other
    /// yield point in that guarded span, and it predates #64 -- `stop`'s
    /// `note_stop` bumps the epoch before it ever needs the slot lock, same as
    /// the OAuth-renewal case, but `start`'s recheck ran BEFORE this await
    /// started, so a stop landing during materialisation sailed past it. The
    /// process does not escape supervision forever -- `stop` still blocks on
    /// the slot lock until `start` stores `Running` and kills it moments later
    /// -- but a process gets spawned and a token gets minted for an agent the
    /// operator already told to stop, which is exactly what the epoch check
    /// exists to prevent (see the comment on the first check, above).
    ///
    /// Mutation-checked: drop the second check and this fails with a spawn.
    /// Same shape as refresh.rs's `a_stop_during_a_renewal_actually_stops_the_
    /// agent`, one guarded window over -- a slow `git` on PATH stands in for
    /// that test's slow fake OAuth CLI.
    #[tokio::test]
    async fn a_stop_during_workspace_materialisation_actually_stops_the_agent() {
        let real_git = String::from_utf8(
            std::process::Command::new("sh")
                .args(["-c", "command -v git"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_string();
        assert!(!real_git.is_empty(), "this test needs a real git on PATH");

        // A real, tiny local repo to clone from -- seeded with the REAL git,
        // before the slow shim below is anywhere near PATH.
        let origin = std::env::temp_dir().join(format!(
            "wheel-ws-race-origin-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&origin).unwrap();
        let run_git = |args: &[&str]| {
            let out = std::process::Command::new(&real_git)
                .args(args)
                .current_dir(&origin)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@example.com")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@example.com")
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}: {out:?}");
        };
        run_git(&["init", "-q", "-b", "main"]);
        std::fs::write(origin.join("README"), "hello").unwrap();
        run_git(&["add", "README"]);
        run_git(&["commit", "-qm", "first"]);
        let url = format!("file://{}", origin.display());

        let (sup, id, dir) = shim_supervisor_cfg("ws-stop-race", ECHO_HARNESS, |cfg| {
            cfg.workspaces = vec![wheel_core::Workspace {
                path: "wheel".into(),
                git: Some(wheel_core::GitSource {
                    url,
                    git_ref: None,
                    vault_ref: None,
                }),
            }];
        });

        // A `git` on PATH that sleeps before the bare clone, so there is a
        // real window to land a stop in.
        let shim_dir = dir.join("shim");
        std::fs::create_dir_all(&shim_dir).unwrap();
        let shim = shim_dir.join("git");
        std::fs::write(
            &shim,
            format!(
                "#!/bin/sh\ncase \"$*\" in\n  *'clone --bare'*) sleep 1.5 ;;\nesac\nexec {real_git} \"$@\"\n"
            ),
        )
        .unwrap();
        std::fs::set_permissions(&shim, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();

        let path_before = std::env::var("PATH").unwrap_or_default();
        let _env = EnvGuard::set(&[("PATH", &format!("{}:{path_before}", shim_dir.display()))]);

        let sup2 = sup.clone();
        let starting = tokio::spawn(async move { sup2.start(id).await });
        // Long enough to be inside the slow clone, which holds the slot lock
        // the whole time -- `stop` below queues on it rather than racing it.
        tokio::time::sleep(std::time::Duration::from_millis(600)).await;

        sup.stop(id).await.unwrap();
        let started = starting.await.unwrap().unwrap();

        assert_eq!(
            started,
            AgentStatus::Stopped,
            "a start that was materialising a workspace when the operator stopped the agent must not spawn"
        );
        assert_eq!(status_of(&sup, id), AgentStatus::Stopped);
        assert!(
            !sup.live_agents().await.contains(&id),
            "the operator's stop must leave no process behind"
        );
        assert_eq!(
            runs(&dir),
            0,
            "no harness process may be spawned after a stop"
        );

        std::fs::remove_dir_all(&origin).ok();
    }

    /// (b) §3c#14, and the reason this project exists rather than YOKE.
    /// Measured before it existed: an agent an hour past its turn still held a
    /// 162 MB claude process.
    #[tokio::test]
    async fn an_idle_agent_parks_and_releases_its_process() {
        let (sup, id, dir) = shim_supervisor_cfg("parks", ECHO_HARNESS, |cfg| {
            cfg.idle_timeout_secs = Some(1);
        });
        sup.start(id).await.unwrap();
        until("the agent to be running", || {
            !matches!(status_of(&sup, id), AgentStatus::Starting)
        })
        .await;
        enqueue(&sup, id, "do a turn");
        sup.deliver(id).await.unwrap();

        until("the agent to park after its idle timeout", || {
            matches!(status_of(&sup, id), AgentStatus::Parked)
        })
        .await;

        assert_eq!(
            runs(&dir),
            1,
            "parking must stop the process it already had, not spawn another"
        );
        let conn = sup.db.lock().unwrap();
        assert!(
            board::agent_state(&conn, id)
                .unwrap_or_default()
                .session_id
                .is_some(),
            "parking must KEEP the session — resume is what makes it free"
        );
    }

    /// wheel-harness-auth.md, "PM ruling: mid-flow enforcement window": an
    /// agent that self-provisions an OAuth credential (`claude auth login`,
    /// run from inside its own turn -- no API route involved) must not keep
    /// running on it indefinitely under `api-key-only`. The periodic
    /// re-check has to catch this even while the agent is otherwise perfectly
    /// idle-timer-eligible, not just at the next natural park/restart.
    #[tokio::test]
    async fn a_running_agent_is_stopped_when_it_goes_oauth_shaped_under_api_key_only() {
        let (sup, id, dir) = shim_supervisor_inner(
            "policy-midflow",
            ECHO_HARNESS,
            |cfg| cfg.idle_timeout_secs = Some(1),
            crate::config::DEFAULT_STARTUP_DEADLINE_SECS,
            None,
            crate::config::HarnessAuthPolicy::ApiKeyOnly,
        );
        sup.start(id).await.unwrap();
        until("the agent to be running", || {
            !matches!(status_of(&sup, id), AgentStatus::Starting)
        })
        .await;
        // No credential yet, so the spawn gate had nothing to refuse. Now the
        // agent does what an API route never sees: it logs itself in.
        enqueue(&sup, id, "do a turn");
        sup.deliver(id).await.unwrap();
        let config_dir = dir.join("creds").join(id.to_string());
        std::fs::write(
            config_dir.join(".credentials.json"),
            r#"{"accessToken":"sk-ant-oat01-selfprovisioned"}"#,
        )
        .unwrap();

        until(
            "the periodic re-check to stop the agent for the policy violation",
            || matches!(status_of(&sup, id), AgentStatus::Error),
        )
        .await;

        assert_eq!(
            runs(&dir),
            1,
            "the re-check kills the process; it must not also restart one"
        );
        let err = {
            let conn = sup.db.lock().unwrap();
            board::agent_state(&conn, id).unwrap().last_error.unwrap()
        };
        assert!(err.contains("api-key-only"), "name the policy: {err}");
        assert!(
            err.contains("mid-turn"),
            "say why this is different from a spawn-time refusal: {err}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The re-check must not fire on nothing -- an `api-key-only` agent with
    /// no OAuth-shaped credential anywhere still parks normally on its own
    /// idle timer, same as under the permissive default.
    #[tokio::test]
    async fn an_api_key_only_agent_with_no_violation_still_parks_normally() {
        let (sup, id, dir) = shim_supervisor_inner(
            "policy-clean-park",
            ECHO_HARNESS,
            |cfg| cfg.idle_timeout_secs = Some(1),
            crate::config::DEFAULT_STARTUP_DEADLINE_SECS,
            None,
            crate::config::HarnessAuthPolicy::ApiKeyOnly,
        );
        sup.start(id).await.unwrap();
        until("the agent to be running", || {
            !matches!(status_of(&sup, id), AgentStatus::Starting)
        })
        .await;
        enqueue(&sup, id, "do a turn");
        sup.deliver(id).await.unwrap();

        until("the agent to park after its idle timeout", || {
            matches!(status_of(&sup, id), AgentStatus::Parked)
        })
        .await;
        std::fs::remove_dir_all(&dir).ok();
    }

    /// An agent with work queued must not be parked out from under it.
    #[tokio::test]
    async fn an_agent_with_queued_work_is_not_parked() {
        let (sup, id, _dir) = shim_supervisor_cfg("busy", SILENT_HARNESS, |cfg| {
            cfg.idle_timeout_secs = Some(1);
        });
        sup.start(id).await.unwrap();
        enqueue(&sup, id, "one");
        enqueue(&sup, id, "two");
        let _ = sup.deliver(id).await;

        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        assert!(
            !matches!(status_of(&sup, id), AgentStatus::Parked),
            "an agent mid-turn with more queued must never be parked"
        );
    }

    /// The sender must be a REAL node: `row_to_message` resolves a `node`
    /// sender to its CURRENT name and type by id, and falls back to `system`
    /// when the node is gone — deliberately, so a deleted node cannot break an
    /// inbox. A fabricated uuid therefore reads back as `system`, which is what
    /// this helper existing separately is meant to stop anyone rediscovering.
    fn enqueue_from_agent(sup: &Supervisor, from: &str, to: Uuid, body: &str) {
        let conn = sup.db.lock().unwrap();
        let sender = wheel_core::Node::new(
            Uuid::new_v4(),
            from.parse().unwrap(),
            wheel_core::Position::default(),
            wheel_core::NodeConfig::Agent(wheel_core::AgentConfig::default()),
        );
        board::create(&conn, &sender).unwrap();
        messages::enqueue(
            &conn,
            wheel_core::MessageSender::Node {
                id: sender.id,
                name: sender.name.clone(),
                node_type: NodeType::Agent,
            },
            to,
            body.to_string(),
            None,
            None,
        )
        .unwrap();
    }

    /// (c) The producer a multi-agent board actually runs on.
    ///
    /// Agent→agent was covered at the CLI plane, and envelope rendering was
    /// covered as a wheel-core unit test, but nothing asserted the two TOGETHER
    /// — every test that watched bytes reach a child's stdin used a `user`
    /// sender. So the lane the swarm lives on was proven in halves.
    #[tokio::test]
    async fn a_message_from_an_agent_reaches_the_child_as_an_agent() {
        let (sup, id, _dir) = shim_supervisor("recipient", ECHO_HARNESS);
        sup.start(id).await.unwrap();
        until("the agent to come up", || {
            !matches!(status_of(&sup, id), AgentStatus::Starting)
        })
        .await;

        enqueue_from_agent(&sup, "researcher", id, "findings attached");
        sup.deliver(id).await.unwrap();

        until("the message to be consumed", || {
            let conn = sup.db.lock().unwrap();
            messages::has_queued(&conn, id).map(|q| !q).unwrap_or(false)
        })
        .await;

        let lines = transcript(&sup, id);
        let joined = lines.join("\n");
        assert!(
            joined.contains("from=\"researcher\""),
            "the child must be told WHICH agent sent it; got: {joined}"
        );
        assert!(
            joined.contains("type=\"agent\""),
            "an agent-sent message must be framed as type=agent, not user — attribution is the \
             whole point of the envelope; got: {joined}"
        );
        assert!(
            joined.contains("findings attached"),
            "the body must reach the child intact; got: {joined}"
        );
    }

    fn transcript(sup: &Supervisor, id: Uuid) -> Vec<String> {
        let conn = sup.db.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT text FROM logs WHERE node_id = ?1 AND stream = 'transcript' ORDER BY seq",
            )
            .unwrap();
        let rows = stmt
            .query_map(rusqlite::params![id.to_string()], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        rows
    }

    /// Records the `resume` the ENGINE put in each spawn spec.
    ///
    /// The shim driver deliberately passes no argv, so an argv-based assertion
    /// measures the stub rather than the engine — my first attempt at this test
    /// did exactly that and failed for its own reasons. What matters is what the
    /// engine hands the driver; `ClaudeDriver::argv` turning `resume` into
    /// `--resume` is its own test.
    struct ResumeRecordingDriver {
        program: String,
        log: std::path::PathBuf,
    }

    impl Harness for ResumeRecordingDriver {
        fn program(&self) -> &str {
            &self.program
        }
        fn argv(&self, spec: &SpawnSpec) -> Vec<std::ffi::OsString> {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.log)
                .unwrap();
            writeln!(f, "{}", spec.resume.as_deref().unwrap_or("<none>")).unwrap();
            Vec::new()
        }
        fn env(&self, _spec: &SpawnSpec) -> Vec<(String, String)> {
            Vec::new()
        }
        fn encode_turn(&self, envelope: &str) -> String {
            format!("{envelope}\n")
        }
        fn parse_line(&self, line: &str) -> HarnessEvent {
            ClaudeDriver.parse_line(line)
        }
        fn classify_startup_failure(&self, _code: Option<i32>, stderr: &str) -> StartupFailure {
            ClaudeDriver.classify_startup_failure(None, stderr)
        }
    }

    const SESSION_HARNESS: &str = r#"#!/bin/sh
echo "{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"sess-abc\"}"
while IFS= read -r line; do
  echo "{\"type\":\"result\",\"subtype\":\"success\",\"session_id\":\"sess-abc\",\"is_error\":false,\"result\":\"ok\"}"
done
"#;

    /// The question parking lives or dies on: after a park, is the next child
    /// handed the SAME session, or a fresh one — which would be a silent
    /// context wipe.
    #[tokio::test]
    async fn a_resumed_agent_is_handed_the_same_session_it_parked_with() {
        let root = std::env::temp_dir().join(format!("wheel-resume-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let log = root.join("resumes");
        std::fs::remove_file(&log).ok();

        let log_for_driver = log.clone();
        let (sup, id, _dir) = shim_supervisor_inner(
            "resumes",
            SESSION_HARNESS,
            |cfg| cfg.idle_timeout_secs = Some(1),
            crate::config::DEFAULT_STARTUP_DEADLINE_SECS,
            Some(Box::new(move |program| {
                Arc::new(ResumeRecordingDriver {
                    program,
                    log: log_for_driver,
                }) as Arc<dyn crate::harness::Harness>
            })),
            crate::config::HarnessAuthPolicy::default(),
        );

        sup.start(id).await.unwrap();
        until("the agent to initialise", || {
            !matches!(status_of(&sup, id), AgentStatus::Starting)
        })
        .await;
        enqueue(&sup, id, "first turn");
        sup.deliver(id).await.unwrap();

        until("the agent to park", || {
            matches!(status_of(&sup, id), AgentStatus::Parked)
        })
        .await;

        enqueue(&sup, id, "second turn");
        sup.deliver(id).await.unwrap();
        until("the agent to be respawned", || {
            std::fs::read_to_string(&log)
                .map(|a| a.lines().count() >= 2)
                .unwrap_or(false)
        })
        .await;

        let recorded = std::fs::read_to_string(&log).unwrap();
        let mut lines = recorded.lines();
        assert_eq!(
            lines.next(),
            Some("<none>"),
            "the FIRST spawn has nothing to resume"
        );
        assert_eq!(
            lines.next(),
            Some("sess-abc"),
            "the resumed child must be handed the session it parked with; a fresh one is a \
             silent context wipe. Recorded: {recorded:?}"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// BUG-040 (QA): a timer is armed per turn and none is cancelled, so an
    /// OLD timer outlives the turn that armed it. Two turns 250s apart under a
    /// 300s timeout leaves the first firing 50s after the second — parking an
    /// agent that was active moments ago and silently shortening the hot window
    /// the operator asked for.
    ///
    /// Asserted by calling `park` directly with a RECENT last_activity: it must
    /// decline and report the remainder rather than park.
    #[tokio::test]
    async fn a_stale_timer_does_not_park_an_agent_that_was_just_active() {
        let (sup, id, _dir) = shim_supervisor_cfg("stale-timer", ECHO_HARNESS, |cfg| {
            cfg.idle_timeout_secs = Some(300);
        });
        sup.start(id).await.unwrap();
        until("the agent to initialise", || {
            matches!(status_of(&sup, id), AgentStatus::Idle)
        })
        .await;

        // last_activity is set by the init that just happened, so "now".
        let left = sup.park(id).await;

        assert!(
            left.is_some(),
            "park must DECLINE for an agent active moments ago, and report what is left"
        );
        let left = left.unwrap();
        assert!(
            left > 250 && left <= 300,
            "the remainder must be measured from last_activity, not restarted from zero: {left}"
        );
        assert!(
            matches!(status_of(&sup, id), AgentStatus::Idle),
            "declining to park must leave the agent exactly as it was"
        );
    }

    /// A start that resumed a session and never reached `init` must not leave
    /// that session in place: nothing else cleared it, so every later start
    /// re-passed the same dead id and failed identically — an agent that could
    /// never recover on its own.
    #[tokio::test]
    async fn a_session_that_never_initialises_is_cleared_so_the_next_start_is_fresh() {
        // Exits immediately without ever printing an init line.
        const NEVER_INITS: &str = "#!/bin/sh\nexit 1\n";
        let (sup, id, _dir) = shim_supervisor("dead-session", NEVER_INITS);

        // Give it a session as though a previous run had established one.
        {
            let conn = sup.db.lock().unwrap();
            set_session(&conn, id, "sess-dead");
        }
        assert!(
            {
                let conn = sup.db.lock().unwrap();
                board::agent_state(&conn, id)
                    .unwrap_or_default()
                    .session_id
                    .is_some()
            },
            "precondition: the agent starts with a session to lose"
        );

        let _ = sup.start(id).await;
        until("the failed start to be reaped", || {
            let conn = sup.db.lock().unwrap();
            board::agent_state(&conn, id)
                .unwrap_or_default()
                .session_id
                .is_none()
        })
        .await;

        let conn = sup.db.lock().unwrap();
        assert!(
            board::agent_state(&conn, id)
                .unwrap_or_default()
                .session_id
                .is_none(),
            "a session that never initialised must be cleared, or the agent retries it forever"
        );
    }

    fn status_of(sup: &Supervisor, id: Uuid) -> AgentStatus {
        let conn = sup.db.lock().unwrap();
        board::agent_state(&conn, id).unwrap_or_default().status
    }

    fn runs(dir: &std::path::Path) -> usize {
        std::fs::read_to_string(dir.join("runs"))
            .map(|s| s.lines().count())
            .unwrap_or(0)
    }

    /// Wait for a condition rather than for a duration: a fixed sleep is a
    /// guess about machine load, and these tests spawn real processes while
    /// the rest of the suite runs beside them.
    async fn until(what: &str, mut cond: impl FnMut() -> bool) {
        // Deliberately generous. These tests spawn REAL child processes on a
        // dev host shared by six agents, where load averages above 15 and a
        // cargo waiting on the build-directory lock are normal. The deadline
        // exists to fail fast when a condition will never hold, not to assert
        // anything about speed -- and at 10s it was reporting healthy code as
        // broken whenever the machine was busy, which is worse than slow.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while std::time::Instant::now() < deadline {
            if cond() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!("timed out waiting for {what}");
    }

    // --- usage windows (parity proposal §1), against the real fake-claude ----

    fn now_unix() -> i64 {
        time::OffsetDateTime::now_utc().unix_timestamp()
    }

    /// A supervisor whose child is `qa/harness/fake-claude`, steered by `fake`
    /// (its config file), on a board of one agent with a usable vault key.
    fn fake_supervisor(
        name: &str,
        fake: serde_json::Value,
        tweak: impl FnOnce(&mut wheel_core::AgentConfig),
    ) -> (Arc<Supervisor>, Uuid, std::path::PathBuf) {
        use base64::Engine;
        let dir = std::env::temp_dir().join(format!(
            "wheel-fake-{name}-{}-{}",
            std::process::id(),
            Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let config = dir.join("fake.json");
        std::fs::write(&config, fake.to_string()).unwrap();

        let conn = crate::db::open_memory().unwrap();
        let mut agent_cfg = wheel_core::AgentConfig {
            system_prompt: "test".into(),
            ..Default::default()
        };
        tweak(&mut agent_cfg);
        let node = wheel_core::Node::new(
            Uuid::new_v4(),
            name.parse().unwrap(),
            wheel_core::Position::default(),
            wheel_core::NodeConfig::Agent(agent_cfg),
        );
        board::create(&conn, &node).unwrap();

        let cfg = Arc::new(Config {
            project_id: Uuid::new_v4(),
            engine_secret: "0123456789abcdef".into(),
            vault_key: Some(base64::engine::general_purpose::STANDARD.encode([7u8; 32])),
            data_dir: dir.clone(),
            listen: wheel_core::ListenAddr::parse("tcp://127.0.0.1:7999").unwrap(),
            json_logs: false,
            tool_allow_hosts: Vec::new(),
            startup_deadline_secs: crate::config::DEFAULT_STARTUP_DEADLINE_SECS,
            harness_auth: crate::config::HarnessAuthPolicy::default(),
        });
        let sup = Arc::new(
            Supervisor::with_harness(
                cfg,
                Arc::new(Mutex::new(conn)),
                Arc::new(crate::events::Bus::new()),
                Arc::new(crate::harness::fake::FakeClaude { config }),
            )
            .with_limit_jitter(0..=0),
        );
        (sup, node.id, dir)
    }

    /// `deliver` resumes a parked agent and never a stopped one (§3c#13).
    fn parked(sup: &Supervisor, id: Uuid) {
        let conn = sup.db.lock().unwrap();
        set_status_db(&conn, id, AgentStatus::Parked, None);
    }

    fn state_of(sup: &Supervisor, id: Uuid) -> wheel_core::AgentState {
        let conn = sup.db.lock().unwrap();
        board::agent_state(&conn, id).unwrap_or_default()
    }

    fn message_to(sup: &Supervisor, id: Uuid) -> Uuid {
        let conn = sup.db.lock().unwrap();
        messages::inbox(&conn, id, None, 10).unwrap()[0].id
    }

    fn settled(sup: &Supervisor, mid: Uuid) -> messages::Settlement {
        sup.settlement_of(mid).expect("the message exists")
    }

    fn limit_requeues(sup: &Supervisor, mid: Uuid) -> i64 {
        let conn = sup.db.lock().unwrap();
        conn.query_row(
            "SELECT limit_requeues FROM messages WHERE id = ?1",
            [mid.to_string()],
            |r| r.get(0),
        )
        .unwrap()
    }

    /// A vault the agent reads, holding one value.
    fn wired_vault(sup: &Supervisor, agent: Uuid, name: &str, key: &str, value: &str) -> Uuid {
        let conn = sup.db.lock().unwrap();
        let v = wheel_core::Node::new(
            Uuid::new_v4(),
            name.parse().unwrap(),
            wheel_core::Position::default(),
            wheel_core::NodeConfig::Vault(wheel_core::VaultConfig {
                keys: vec![key.into()],
            }),
        );
        board::create(&conn, &v).unwrap();
        board::add_wire(&conn, agent, v.id, wheel_core::WireType::Read, None).unwrap();
        crate::vault::put(&conn, sup.vault_key().unwrap(), v.id, key, value).unwrap();
        v.id
    }

    fn choose_fallback(sup: &Supervisor, agent: Uuid, vault: Uuid) {
        let conn = sup.db.lock().unwrap();
        let mut node = board::get(&conn, agent).unwrap().unwrap();
        if let wheel_core::NodeConfig::Agent(a) = &mut node.config {
            a.fallback_vault = Some(vault);
        }
        board::update(&conn, &node).unwrap();
    }

    /// One record per spawn, from the fake's credential-routing dump.
    fn spawns(dump: &std::path::Path) -> Vec<serde_json::Value> {
        std::fs::read_to_string(dump)
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    fn credential_sha(spawn: &serde_json::Value, var: &str) -> Option<String> {
        spawn["credentials"][var]["sha256"].as_str().map(str::to_string)
    }

    /// The whole of item 1's first half: the window closes mid-work, the
    /// message is requeued rather than lost, the agent parks with the reset on
    /// its state, and it comes back at the reset -- not before -- and finishes.
    #[tokio::test]
    async fn a_closed_usage_window_requeues_parks_and_resumes_at_the_reset() {
        let reopens = now_unix() + 4;
        let (sup, id, dir) = fake_supervisor("quota-park", serde_json::Value::Null, |_| {});
        let dump = dir.join("spawns.jsonl");
        std::fs::write(
            dir.join("fake.json"),
            serde_json::json!({ "limit_until": reopens, "env_dump": dump }).to_string(),
        )
        .unwrap();
        parked(&sup, id);
        enqueue(&sup, id, "the work <<FAKE:REPLY=done after the reset>>");
        sup.deliver(id).await.unwrap();

        until("the agent to park on the closed window", || {
            status_of(&sup, id) == AgentStatus::RateLimited
        })
        .await;
        let state = state_of(&sup, id);
        assert_eq!(
            state.resets_at.map(|t| t.into_inner().unix_timestamp()),
            Some(reopens),
            "the reset is the one the harness's rate_limit_event carried"
        );
        assert_eq!(state.resume_at, state.resets_at, "zero jitter in tests");
        assert_eq!(state.quota.as_ref().map(|q| q.status.as_str()), Some("rejected"));
        let mid = message_to(&sup, id);
        let s = settled(&sup, mid);
        assert_eq!(
            s.state,
            MessageState::Queued,
            "the turn never completed, so the message goes back rather than being consumed"
        );
        assert!(s.last_error.unwrap_or_default().contains("rate limited"));
        assert_eq!(limit_requeues(&sup, mid), 1);
        assert!(!has_process(&sup, id).await, "a parked agent holds no process");

        until("the requeued message to be consumed after the reset", || {
            settled(&sup, mid).state == MessageState::Consumed
        })
        .await;
        assert!(
            now_unix() >= reopens,
            "it must not come back before the window reopens"
        );
        assert_eq!(
            settled(&sup, mid).result.as_deref(),
            Some("done after the reset")
        );
        assert_eq!(status_of(&sup, id), AgentStatus::Idle);
        assert_eq!(
            spawns(&dump).len(),
            2,
            "one spawn into the closed window, one after it: an agent that retried early would \
             have spent processes and turns against a window the harness said was shut"
        );
        assert_eq!(limit_requeues(&sup, mid), 1);
        sup.stop(id).await.ok();
        std::fs::remove_dir_all(&dir).ok();
    }

    /// With no `rate_limit_event`, the reset comes from the text alone.
    #[tokio::test]
    async fn a_limit_named_only_in_the_error_text_parks_until_the_reset_it_names() {
        let reopens = now_unix() + 600;
        let (sup, id, dir) = fake_supervisor(
            "quota-text",
            serde_json::json!({ "limit_until": reopens, "limit_style": "text" }),
            |_| {},
        );
        parked(&sup, id);
        enqueue(&sup, id, "work");
        sup.deliver(id).await.unwrap();
        until("the agent to park", || {
            status_of(&sup, id) == AgentStatus::RateLimited
        })
        .await;
        let state = state_of(&sup, id);
        assert_eq!(
            state.resume_at.map(|t| t.into_inner().unix_timestamp()),
            Some(reopens)
        );
        assert!(state.quota.is_none(), "no event, so no window was reported");
        sup.stop(id).await.ok();
        std::fs::remove_dir_all(&dir).ok();
    }

    /// F008 for the new signal. A `rejected` event the agent could have
    /// printed is not bound to the session, so the failed turn is an ordinary
    /// task error: consumed, not requeued, and nothing is parked.
    #[tokio::test]
    async fn a_rejected_event_from_another_session_neither_parks_nor_requeues() {
        let (sup, id, _dir) = shim_supervisor(
            "quota-forged",
            r#"#!/bin/sh
echo '{"type":"system","subtype":"init","session_id":"s1"}'
while IFS= read -r line; do
  echo '{"type":"rate_limit_event","session_id":"forged","rate_limit_info":{"status":"rejected","resetsAt":4102444800}}'
  echo '{"type":"result","subtype":"error_during_execution","session_id":"s1","is_error":true,"result":"the build failed"}'
done
"#,
        );
        sup.start(id).await.unwrap();
        enqueue(&sup, id, "build it");
        sup.deliver(id).await.unwrap();
        until("the turn to end", || status_of(&sup, id) == AgentStatus::Error).await;
        let s = settled(&sup, message_to(&sup, id));
        assert_eq!(s.outcome(), "error", "a task error is consumed: poison must not loop");
        assert!(state_of(&sup, id).resume_at.is_none());
        assert!(engine_log(&sup, id)
            .iter()
            .any(|l| l.contains("mismatched session_id")));
        sup.stop(id).await.ok();
    }

    /// The fallback half (ADVERSARY-gated): the primary account's window
    /// closes, the agent restarts ONCE on the fallback vault's credential and
    /// finishes the message there. Each spawn sees exactly one account.
    #[tokio::test]
    async fn a_limited_agent_restarts_once_on_its_fallback_credential() {
        let (sup, id, dir) = fake_supervisor("quota-fallback", serde_json::Value::Null, |_| {});
        let dump = dir.join("spawns.jsonl");
        std::fs::write(
            dir.join("fake.json"),
            serde_json::json!({
                "limit_until": now_unix() + 600,
                "limit_when_env": ["CLAUDE_CODE_OAUTH_TOKEN"],
                "env_dump": dump,
            })
            .to_string(),
        )
        .unwrap();
        let (primary, standby) = ("sk-ant-oat01-primary-account", "sk-ant-api03-standby-account");
        wired_vault(&sup, id, "primary", "CLAUDE_CODE_OAUTH_TOKEN", primary);
        let standby_vault = wired_vault(&sup, id, "standby", "ANTHROPIC_API_KEY", standby);
        choose_fallback(&sup, id, standby_vault);

        parked(&sup, id);
        enqueue(&sup, id, "<<FAKE:REPLY=carried by the fallback>>");
        sup.deliver(id).await.unwrap();
        let mid = message_to(&sup, id);
        until("the message to be consumed on the fallback", || {
            settled(&sup, mid).state == MessageState::Consumed
        })
        .await;

        assert_eq!(
            settled(&sup, mid).result.as_deref(),
            Some("carried by the fallback")
        );
        assert!(state_of(&sup, id).fallback_until.is_some());
        let runs = spawns(&dump);
        assert_eq!(runs.len(), 2, "one spawn on the primary, one on the fallback");
        let sha = |v: &str| Some(wheel_core::sha256_hex(v.as_bytes()));
        assert_eq!(credential_sha(&runs[0], "CLAUDE_CODE_OAUTH_TOKEN"), sha(primary));
        assert_eq!(
            credential_sha(&runs[0], "ANTHROPIC_API_KEY"),
            None,
            "a normal spawn must not also carry the standby account"
        );
        assert_eq!(credential_sha(&runs[1], "ANTHROPIC_API_KEY"), sha(standby));
        assert_eq!(
            credential_sha(&runs[1], "CLAUDE_CODE_OAUTH_TOKEN"),
            None,
            "a fallback spawn carries the fallback account and nothing else"
        );
        sup.stop(id).await.ok();
        std::fs::remove_dir_all(&dir).ok();
    }

    /// "Once, then parks": two closed accounts are a park, never a bounce.
    #[tokio::test]
    async fn a_fallback_that_is_limited_too_parks_rather_than_bouncing() {
        let (sup, id, dir) = fake_supervisor("quota-both", serde_json::Value::Null, |_| {});
        let dump = dir.join("spawns.jsonl");
        std::fs::write(
            dir.join("fake.json"),
            serde_json::json!({
                "limit_until": now_unix() + 600,
                "limit_when_env": ["CLAUDE_CODE_OAUTH_TOKEN", "ANTHROPIC_API_KEY"],
                "env_dump": dump,
            })
            .to_string(),
        )
        .unwrap();
        wired_vault(&sup, id, "primary", "CLAUDE_CODE_OAUTH_TOKEN", "sk-ant-oat01-a");
        let standby = wired_vault(&sup, id, "standby", "ANTHROPIC_API_KEY", "sk-ant-api03-b");
        choose_fallback(&sup, id, standby);

        parked(&sup, id);
        enqueue(&sup, id, "work");
        sup.deliver(id).await.unwrap();
        until("both accounts to be tried and the agent parked", || {
            status_of(&sup, id) == AgentStatus::RateLimited
        })
        .await;
        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
        assert_eq!(spawns(&dump).len(), 2, "primary, then fallback, then park");
        let state = state_of(&sup, id);
        assert!(state.resume_at <= state.fallback_until);
        assert_eq!(settled(&sup, message_to(&sup, id)).state, MessageState::Queued);
        sup.stop(id).await.ok();
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Credential-distribution rule: a token the agent wrote into its own
    /// config dir must not ride along into a fallback spawn. Planted as the
    /// OTHER credential kind, so the fallback vault's value cannot mask it by
    /// overwriting the same variable.
    #[tokio::test]
    async fn a_token_planted_in_the_agents_own_dir_never_rides_a_fallback_spawn() {
        let (sup, id, dir) = fake_supervisor("quota-planted", serde_json::Value::Null, |_| {});
        let dump = dir.join("spawns.jsonl");
        std::fs::write(
            dir.join("fake.json"),
            serde_json::json!({
                "limit_until": now_unix() + 600,
                "limit_when_env": ["CLAUDE_CODE_OAUTH_TOKEN"],
                "env_dump": dump,
            })
            .to_string(),
        )
        .unwrap();
        let planted = "sk-ant-oat01-planted-by-the-agent";
        crate::auth::store_token(
            &dir.join("creds").join(id.to_string()),
            planted,
            wheel_core::Harness::Claude,
        )
        .unwrap();
        let standby = wired_vault(&sup, id, "standby", "ANTHROPIC_API_KEY", "sk-ant-api03-ok");
        choose_fallback(&sup, id, standby);

        parked(&sup, id);
        enqueue(&sup, id, "<<FAKE:REPLY=clean>>");
        sup.deliver(id).await.unwrap();
        let mid = message_to(&sup, id);
        until("the message to finish on the fallback", || {
            settled(&sup, mid).state == MessageState::Consumed
        })
        .await;

        let runs = spawns(&dump);
        let planted_sha = wheel_core::sha256_hex(planted.as_bytes());
        assert_eq!(
            credential_sha(&runs[0], "CLAUDE_CODE_OAUTH_TOKEN").as_deref(),
            Some(planted_sha.as_str()),
            "premise: a normal spawn does carry the node's stored token"
        );
        let fallback_values: Vec<&str> = runs[1]["env_digests"]
            .as_object()
            .unwrap()
            .values()
            .filter_map(|v| v.as_str())
            .collect();
        assert!(
            !fallback_values.contains(&planted_sha.as_str()),
            "the planted token reached the fallback spawn: {:?}",
            runs[1]["credential_vars_set"]
        );
        sup.stop(id).await.ok();
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Config time is one door; a wire removed afterwards walks past it. The
    /// spawn re-check is the one that always runs.
    #[tokio::test]
    async fn a_fallback_whose_wire_was_removed_is_not_used() {
        let (sup, id, dir) = fake_supervisor("quota-unwired", serde_json::Value::Null, |_| {});
        let dump = dir.join("spawns.jsonl");
        std::fs::write(
            dir.join("fake.json"),
            serde_json::json!({ "limit_until": now_unix() + 600, "env_dump": dump }).to_string(),
        )
        .unwrap();
        let standby = wired_vault(&sup, id, "standby", "ANTHROPIC_API_KEY", "sk-ant-api03-x");
        choose_fallback(&sup, id, standby);
        {
            let conn = sup.db.lock().unwrap();
            board::remove_wire(&conn, id, standby, wheel_core::WireType::Read).unwrap();
        }

        parked(&sup, id);
        enqueue(&sup, id, "work");
        sup.deliver(id).await.unwrap();
        until("the agent to park without a fallback", || {
            status_of(&sup, id) == AgentStatus::RateLimited
        })
        .await;
        assert_eq!(spawns(&dump).len(), 1, "no second spawn on an unwired vault");
        assert!(engine_log(&sup, id)
            .iter()
            .any(|l| l.contains("no longer has a read wire")));
        sup.stop(id).await.ok();
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The bound on the requeue (proposal §1.3): past the cap the message is
    /// consumed with an error, even though the window is still closed.
    #[tokio::test]
    async fn past_the_requeue_cap_a_limited_message_is_consumed_with_an_error() {
        let (sup, id, dir) = fake_supervisor(
            "quota-cap",
            serde_json::json!({ "limit_until": now_unix() + 600 }),
            |_| {},
        );
        parked(&sup, id);
        enqueue(&sup, id, "work");
        let mid = message_to(&sup, id);
        {
            let conn = sup.db.lock().unwrap();
            conn.execute(
                "UPDATE messages SET limit_requeues = ?2 WHERE id = ?1",
                rusqlite::params![mid.to_string(), quota::MAX_LIMIT_REQUEUES],
            )
            .unwrap();
        }
        sup.deliver(id).await.unwrap();
        until("the message to be given up on", || {
            settled(&sup, mid).is_terminal()
        })
        .await;
        let s = settled(&sup, mid);
        assert_eq!(s.outcome(), "error");
        assert!(s.last_error.unwrap_or_default().contains("13 times"));
        until("the agent to park", || {
            status_of(&sup, id) == AgentStatus::RateLimited
        })
        .await;
        sup.stop(id).await.ok();
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The timer lives in memory and the resume time in the database, so a
    /// restart re-arms from the database -- and does not "helpfully" park a
    /// run_on_startup agent whose window is still closed.
    #[tokio::test]
    async fn an_engine_restart_rearms_the_resume_and_keeps_a_closed_window_closed() {
        let (sup, id, dir) =
            fake_supervisor("quota-boot", serde_json::json!({}), |a| a.run_on_startup = true);
        let (later, later_id, later_dir) = fake_supervisor(
            "quota-boot-later",
            serde_json::json!({}),
            |a| a.run_on_startup = true,
        );
        let ago = time::OffsetDateTime::now_utc() - time::Duration::seconds(1);
        let ahead = time::OffsetDateTime::now_utc() + time::Duration::seconds(600);
        {
            let conn = sup.db.lock().unwrap();
            board::set_rate_limited(&conn, id, None, ago.into(), "closed");
        }
        {
            let conn = later.db.lock().unwrap();
            board::set_rate_limited(&conn, later_id, None, ahead.into(), "closed");
        }
        enqueue(&sup, id, "<<FAKE:REPLY=back after the restart>>");
        enqueue(&later, later_id, "not yet");

        sup.start_configured_agents().await;
        later.start_configured_agents().await;

        let mid = message_to(&sup, id);
        until("the overdue agent to resume and finish", || {
            settled(&sup, mid).state == MessageState::Consumed
        })
        .await;
        assert_eq!(
            settled(&sup, mid).result.as_deref(),
            Some("back after the restart")
        );
        assert_eq!(
            status_of(&later, later_id),
            AgentStatus::RateLimited,
            "boot must not park an agent whose window is still closed"
        );
        sup.stop(id).await.ok();
        std::fs::remove_dir_all(&dir).ok();
        std::fs::remove_dir_all(&later_dir).ok();
    }

    /// An operator's stop wins over a timer already armed: no cancellation,
    /// just a check when it fires.
    #[tokio::test]
    async fn a_stopped_agent_is_not_resumed_by_its_old_timer() {
        let (sup, id, dir) = fake_supervisor("quota-stale", serde_json::json!({}), |_| {});
        let soon = time::OffsetDateTime::now_utc() + time::Duration::seconds(1);
        {
            let conn = sup.db.lock().unwrap();
            board::set_rate_limited(&conn, id, None, soon.into(), "closed");
        }
        enqueue(&sup, id, "must wait for an operator start");
        sup.arm_resume_timer(id, soon);
        sup.stop(id).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(2500)).await;
        assert_eq!(status_of(&sup, id), AgentStatus::Stopped);
        assert!(!has_process(&sup, id).await, "nothing was spawned");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The slot, not `live_agents`: that lists every slot ever touched,
    /// empty ones included.
    async fn has_process(sup: &Supervisor, id: Uuid) -> bool {
        let slot = sup.slot(id).await;
        let held = slot.lock().await.is_some();
        held
    }

    /// The operator's actual first session: start, discover the agent needs
    /// credentials, paste them, start again. If the dead child's slot is not
    /// cleared, that second start returns 200 and spawns NOTHING — the agent
    /// can never be authenticated, and the API says everything is fine.
    #[tokio::test]
    async fn an_agent_can_be_started_again_after_its_child_died() {
        let (sup, id, dir) = shim_supervisor(
            "restartable",
            "#!/bin/sh\necho run >> \"$(dirname \"$0\")/runs\"\n\
             echo 'Invalid API key · Please run /login' >&2\nexit 1\n",
        );
        sup.start(id).await.unwrap();
        until("the agent to report needs_auth", || {
            status_of(&sup, id) == AgentStatus::NeedsAuth
        })
        .await;
        assert_eq!(runs(&dir), 1);

        // The operator authenticates and starts it again.
        sup.start(id).await.unwrap();
        until(
            "the second start to actually spawn a child, not silently do nothing",
            || runs(&dir) == 2,
        )
        .await;
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A child that exits with nothing to say has not failed — a clean stop
    /// looks exactly like this, and reporting it as `error` would light up the
    /// board every time an agent shut down normally.
    #[tokio::test]
    async fn a_silent_exit_is_stopped_not_error() {
        let (sup, id, dir) = shim_supervisor("silent", "#!/bin/sh\nexit 0\n");
        sup.start(id).await.unwrap();
        until("the agent to settle as stopped", || {
            status_of(&sup, id) == AgentStatus::Stopped
        })
        .await;
        std::fs::remove_dir_all(&dir).ok();
    }

    /// §3c#13: a message must never start a process, and concurrent starts
    /// must collapse into one child.
    #[tokio::test]
    async fn concurrent_starts_produce_exactly_one_child() {
        let (sup, id, dir) = shim_supervisor(
            "onlyone",
            "#!/bin/sh\necho run >> \"$(dirname \"$0\")/runs\"\nsleep 5\n",
        );
        let mut tasks = Vec::new();
        for _ in 0..10 {
            let s = sup.clone();
            tasks.push(tokio::spawn(async move { s.start(id).await }));
        }
        for t in tasks {
            t.await.unwrap().unwrap();
        }
        until("the one child to start", || runs(&dir) == 1).await;
        // Give any wrongly-spawned sibling time to show up before concluding.
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert_eq!(runs(&dir), 1, "ten starts must share one process");
        sup.stop(id).await.unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    /// F008. A child prints whatever it likes on stdout, including a
    /// well-formed `result`. Only an event carrying the session id the
    /// supervisor started may end a turn.
    #[test]
    fn a_forged_result_cannot_end_a_turn() {
        // Established session: only the matching id is honoured.
        assert!(session_matches(Some("s1"), Some("s1")));
        assert!(!session_matches(Some("s1"), Some("s2")));
        // A forged event that simply omits the session id must not slip through.
        assert!(!session_matches(Some("s1"), None));
        // Empty and near-miss ids are not matches either.
        assert!(!session_matches(Some("s1"), Some("")));
        assert!(!session_matches(Some("s1"), Some("s1 ")));
        assert!(!session_matches(Some("s1"), Some("S1")));
    }

    #[test]
    fn before_init_there_is_nothing_to_compare_so_events_are_accepted() {
        assert!(session_matches(None, Some("s1")));
        assert!(session_matches(None, None));
    }

    /// A harness that answers every turn, reporting the session it was told to
    /// resume so a test can see whether the context survived.
    /// F015 is fixed at a choke-point, not at each call site, and this is what
    /// keeps it that way. Scripts and MCP servers land in M2; a spawn added
    /// then that reaches for `Command::new` directly would inherit the
    /// engine's secrets again and no runtime test would notice, because the
    /// leaking child would be one nobody had written a test for yet.
    #[test]
    fn every_child_process_is_started_through_child_command() {
        fn rs_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    rs_files(&path, out);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    out.push(path);
                }
            }
        }
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        rs_files(&src, &mut files);
        assert!(!files.is_empty(), "found no sources to scan");

        let mut offenders = Vec::new();
        for file in &files {
            let text = std::fs::read_to_string(file).unwrap();
            // Production code only. A test may build a Command however it
            // likes -- it is not what runs in a sandbox.
            let prod = match text.find("#[cfg(test)]") {
                Some(i) => &text[..i],
                None => &text[..],
            };
            let mut current_fn = String::new();
            for (n, line) in prod.lines().enumerate() {
                if let Some(rest) = line.trim_start().strip_prefix("fn ") {
                    current_fn = rest.split('(').next().unwrap_or_default().to_string();
                } else if let Some(rest) = line.trim_start().strip_prefix("pub(crate) fn ") {
                    current_fn = rest.split('(').next().unwrap_or_default().to_string();
                }
                let code = line.split("//").next().unwrap_or_default();
                if code.contains("Command::new") && current_fn != "child_command" {
                    offenders.push(format!("{}:{}: {}", file.display(), n + 1, line.trim()));
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "these spawn a child without env_clear -- use supervisor::child_command:\n{}",
            offenders.join("\n")
        );
    }

    /// PM/Web: a lapsed credential must surface as `needs_auth`, cleanly.
    ///
    /// A session credential copied into a vault expires for every agent
    /// reading it at once. Spawning anyway means the harness fails on its
    /// first request and reports something of its own devising, so the
    /// operator sees a broken agent instead of one that needs a login -- and
    /// pays for a process to find that out.
    #[tokio::test]
    async fn an_expired_vault_credential_is_needs_auth_before_anything_spawns() {
        let (sup, id, dir) = shim_supervisor("lapsed", ECHO_HARNESS);

        let vk = {
            use base64::Engine as _;
            crate::vault::VaultKey::from_base64(
                &base64::engine::general_purpose::STANDARD.encode([9u8; 32]),
            )
            .unwrap()
        };
        {
            let conn = sup.db.lock().unwrap();
            let v = wheel_core::Node::new(
                Uuid::new_v4(),
                "anthropic".parse().unwrap(),
                wheel_core::Position::default(),
                wheel_core::NodeConfig::Vault(wheel_core::VaultConfig { keys: vec![] }),
            );
            board::create(&conn, &v).unwrap();
            board::add_wire(&conn, id, v.id, wheel_core::WireType::Read, None).unwrap();
            let past = wheel_core::Timestamp::parse_rfc3339("2020-01-01T00:00:00Z").unwrap();
            crate::vault::put_with_expiry(
                &conn,
                &vk,
                v.id,
                "CLAUDE_CODE_OAUTH_TOKEN",
                "stale",
                Some(past),
            )
            .unwrap();
        }

        assert_eq!(sup.start(id).await.unwrap(), AgentStatus::NeedsAuth);
        assert_eq!(status_of(&sup, id), AgentStatus::NeedsAuth);
        assert_eq!(
            runs(&dir),
            0,
            "an expired credential must not spawn a child"
        );

        // The operator is told which vault, and what to do about it.
        let err = {
            let conn = sup.db.lock().unwrap();
            board::agent_state(&conn, id).unwrap().last_error.unwrap()
        };
        assert!(err.contains("anthropic"), "name the vault: {err}");
        assert!(err.contains("setup-token"), "name the durable fix: {err}");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// docs/proposals/wheel-harness-auth.md, enforcement point 4: on an
    /// `api-key-only` project, an OAuth-shaped credential must refuse to
    /// spawn at all -- not `needs_auth` (which implies "log in and it'll
    /// work"), `error`, naming the policy.
    #[tokio::test]
    async fn an_oauth_credential_refuses_to_spawn_under_api_key_only() {
        let (sup, id, dir) = shim_supervisor_api_key_only("policy-oauth", ECHO_HARNESS);
        let config_dir = dir.join("creds").join(id.to_string());
        crate::auth::store_token(&config_dir, "sk-ant-oat01-abc", wheel_core::Harness::Claude)
            .unwrap();

        assert_eq!(sup.start(id).await.unwrap(), AgentStatus::Error);
        assert_eq!(status_of(&sup, id), AgentStatus::Error);
        assert_eq!(runs(&dir), 0, "an OAuth credential must not spawn a child");

        let err = {
            let conn = sup.db.lock().unwrap();
            board::agent_state(&conn, id).unwrap().last_error.unwrap()
        };
        assert!(err.contains("api-key-only"), "name the policy: {err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The same project, an API key instead -- the policy must not be a
    /// blanket refusal to start.
    #[tokio::test]
    async fn an_api_key_starts_normally_under_api_key_only() {
        let (sup, id, dir) = shim_supervisor_api_key_only("policy-apikey", ECHO_HARNESS);
        let config_dir = dir.join("creds").join(id.to_string());
        crate::auth::store_token(&config_dir, "sk-ant-api03-abc", wheel_core::Harness::Claude)
            .unwrap();

        sup.start(id).await.unwrap();
        until("the agent to start", || runs(&dir) > 0).await;
        assert_ne!(status_of(&sup, id), AgentStatus::Error);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A board with no policy set at all (the permissive default) must keep
    /// accepting an OAuth credential exactly as it does today -- upgrading
    /// the engine binary must not change an existing board's behaviour.
    #[tokio::test]
    async fn an_oauth_credential_still_starts_when_no_policy_is_set() {
        let (sup, id, dir) = shim_supervisor("policy-unset", ECHO_HARNESS);
        let config_dir = dir.join("creds").join(id.to_string());
        crate::auth::store_token(&config_dir, "sk-ant-oat01-abc", wheel_core::Harness::Claude)
            .unwrap();

        sup.start(id).await.unwrap();
        until("the agent to start", || runs(&dir) > 0).await;
        assert_ne!(status_of(&sup, id), AgentStatus::Error);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// ...and a credential with NO recorded expiry starts normally. "Nobody
    /// told us" is not "it has lapsed", and refusing on a guess would strand
    /// an agent whose credential is perfectly good.
    #[tokio::test]
    async fn a_credential_without_a_recorded_expiry_still_starts() {
        let (sup, id, dir) = shim_supervisor("no-expiry", ECHO_HARNESS);

        let vk = {
            use base64::Engine as _;
            crate::vault::VaultKey::from_base64(
                &base64::engine::general_purpose::STANDARD.encode([9u8; 32]),
            )
            .unwrap()
        };
        {
            let conn = sup.db.lock().unwrap();
            let v = wheel_core::Node::new(
                Uuid::new_v4(),
                "anthropic".parse().unwrap(),
                wheel_core::Position::default(),
                wheel_core::NodeConfig::Vault(wheel_core::VaultConfig { keys: vec![] }),
            );
            board::create(&conn, &v).unwrap();
            board::add_wire(&conn, id, v.id, wheel_core::WireType::Read, None).unwrap();
            crate::vault::put(&conn, &vk, v.id, "CLAUDE_CODE_OAUTH_TOKEN", "fine").unwrap();
        }

        assert_ne!(sup.start(id).await.unwrap(), AgentStatus::NeedsAuth);
        until("the child to start", || runs(&dir) == 1).await;
        sup.stop(id).await.ok();
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Sets environment variables for the life of a test and puts them back.
    ///
    /// Env is process-global; cargo runs tests in parallel in one process. A
    /// test that sets a variable and walks away is changing the world for
    /// every test scheduled after it, which shows up as failures somewhere
    /// else entirely and only under parallelism.
    struct EnvGuard {
        previous: Vec<(String, Option<String>)>,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    impl EnvGuard {
        fn set(vars: &[(&str, &str)]) -> Self {
            let lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let previous = vars
                .iter()
                .map(|(k, _)| ((*k).to_string(), std::env::var(k).ok()))
                .collect();
            for (k, v) in vars {
                std::env::set_var(k, v);
            }
            Self {
                previous,
                _lock: lock,
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (k, v) in &self.previous {
                match v {
                    Some(v) => std::env::set_var(k, v),
                    None => std::env::remove_var(k),
                }
            }
        }
    }

    /// QA's BUG-021 / 029. The gate asserts the VALUE and the MODE, not that
    /// the variable is present — which is exactly why they caught this and a
    /// membership check would not have.
    #[tokio::test]
    async fn the_crate_cache_is_per_project_and_private_to_it() {
        use std::os::unix::fs::PermissionsExt;
        let (sup, _id, dir) = shim_supervisor("cargo-home", ECHO_HARNESS);

        let cargo_home = sup.project_cache(".cargo").unwrap();

        // Under the project's OWN data dir. On the process backend that dir is
        // /data/projects/<id>, so this is 029's path; the old `data_dir/cargo`
        // sat beside whatever else shared a host data dir.
        assert!(
            cargo_home.starts_with(&dir),
            "the cache must live under this project's data dir: {}",
            cargo_home.display()
        );
        assert_eq!(cargo_home.file_name().unwrap(), ".cargo");

        // 0700. `create_dir_all` alone gives 0755, and every other uid in the
        // sandbox could then read fetched sources and any credentials.toml.
        let mode = std::fs::metadata(&cargo_home).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o700,
            "the crate cache is mode {mode:o}; it holds registry credentials"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// QA's WOW-toolchain-cargo-owned. Every other check here follows
    /// symlinks, so a `.cargo` pointing into a shared directory passes the
    /// path check and the mode check and shares the cache anyway.
    #[tokio::test]
    async fn a_symlinked_crate_cache_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let (sup, _id, dir) = shim_supervisor("cargo-symlink", ECHO_HARNESS);

        let shared = dir.join("shared-cache");
        std::fs::create_dir_all(&shared).unwrap();
        std::fs::set_permissions(&shared, PermissionsExt::from_mode(0o700)).unwrap();
        let cache = dir.join(".cargo");
        std::fs::remove_dir_all(&cache).ok();
        std::os::unix::fs::symlink(&shared, &cache).unwrap();

        let err = sup.project_cache(".cargo").unwrap_err().to_string();
        assert!(
            err.contains("symlink"),
            "a symlinked cache must be refused, and named: {err}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// QA's WOW-toolchain-cargo-distinct. Value and mode on ONE project cannot
    /// tell a private cache from one shared dir handed to everybody: 0700 and
    /// "looks per-project" are both true of a single shared cache.
    #[tokio::test]
    async fn two_projects_get_different_crate_caches() {
        let (a, _, dir_a) = shim_supervisor("cargo-p1", ECHO_HARNESS);
        let (b, _, dir_b) = shim_supervisor("cargo-p2", ECHO_HARNESS);

        let (ca, cb) = (
            a.project_cache(".cargo").unwrap(),
            b.project_cache(".cargo").unwrap(),
        );
        assert_ne!(ca, cb, "two projects were handed the same crate cache");

        std::fs::remove_dir_all(&dir_a).ok();
        std::fs::remove_dir_all(&dir_b).ok();
    }

    /// A directory that already exists keeps the mode it was made with, so
    /// creating it correctly is not the same as finding it correct: the mode
    /// is SET on every start, not left to whatever made the directory. It is
    /// repaired rather than refused because the cache is ours to fix and a
    /// board that will not start is the worse failure -- but if we cannot
    /// make it private (another uid owns it), `set_permissions` fails and the
    /// agent does not start with it.
    #[tokio::test]
    async fn a_loosened_crate_cache_is_tightened_before_the_child_starts() {
        use std::os::unix::fs::PermissionsExt;
        let (sup, id, dir) = shim_supervisor("cargo-loose", ENV_DUMP_HARNESS);

        let cargo_home = sup.project_cache(".cargo").unwrap();
        std::fs::write(cargo_home.join("credentials.toml"), "token = \"secret\"").unwrap();
        std::fs::set_permissions(&cargo_home, PermissionsExt::from_mode(0o755)).unwrap();

        sup.start(id).await.unwrap();
        let dumped = dir.join("child-env");
        until("the child to report its environment", || dumped.exists()).await;

        let mode = std::fs::metadata(&cargo_home).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "the loosened cache was left at {mode:o}");
        assert!(
            cargo_home.join("credentials.toml").exists(),
            "tightening the cache must not discard what is in it"
        );

        // The gate asserts the VALUE the child was given, not that the name
        // is present: pointing every project at one shared cache would set
        // the variable just as well.
        let env = std::fs::read_to_string(&dumped).unwrap();
        assert!(
            env.lines()
                .any(|l| l == format!("CARGO_HOME={}", cargo_home.display())),
            "the child was not given this project's own cache:\n{env}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// The ENOSPC/memory-pressure incident this fixes: `CARGO_TARGET_DIR` and
    /// `TMPDIR` must be OFF the project's data dir, unlike `CARGO_HOME` above
    /// which deliberately stays on it. A build's object files run to
    /// gigabytes on a volume sized for `wheel.db`.
    #[tokio::test]
    async fn the_scratch_dirs_are_off_the_data_volume_and_private() {
        use std::os::unix::fs::PermissionsExt;
        let (sup, _id, dir) = shim_supervisor("scratch-off-data", ECHO_HARNESS);

        for scratch in [sup.cargo_target_dir().unwrap(), sup.tmp_dir().unwrap()] {
            assert!(
                !scratch.starts_with(&dir),
                "a build scratch dir must not live on the (small) data volume: {}",
                scratch.display()
            );
            let mode = std::fs::metadata(&scratch).unwrap().permissions().mode() & 0o777;
            assert_eq!(
                mode,
                0o700,
                "{} is mode {mode:o}; every uid in the sandbox could read a build's output",
                scratch.display()
            );
        }

        std::fs::remove_dir_all(sup.scratch_root()).ok();
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Same reasoning as `two_projects_get_different_crate_caches`: two
    /// projects sharing a scratch dir would build each other's code in it.
    #[tokio::test]
    async fn two_projects_get_different_scratch_dirs() {
        let (a, _, dir_a) = shim_supervisor("scratch-p1", ECHO_HARNESS);
        let (b, _, dir_b) = shim_supervisor("scratch-p2", ECHO_HARNESS);

        assert_ne!(a.cargo_target_dir().unwrap(), b.cargo_target_dir().unwrap());
        assert_ne!(a.tmp_dir().unwrap(), b.tmp_dir().unwrap());

        std::fs::remove_dir_all(a.scratch_root()).ok();
        std::fs::remove_dir_all(b.scratch_root()).ok();
        std::fs::remove_dir_all(&dir_a).ok();
        std::fs::remove_dir_all(&dir_b).ok();
    }

    /// The gate that matters: not that `cargo_target_dir()`/`tmp_dir()` compute
    /// something reasonable, but that the SPAWNED CHILD actually receives it —
    /// the same distinction QA drew for `CARGO_HOME` (BUG-021/029).
    #[tokio::test]
    async fn the_child_actually_receives_the_off_volume_scratch_dirs() {
        let (sup, id, dir) = shim_supervisor("scratch-child-env", ENV_DUMP_HARNESS);
        let target_dir = sup.cargo_target_dir().unwrap();
        let tmp_dir = sup.tmp_dir().unwrap();

        sup.start(id).await.unwrap();
        let dumped = dir.join("child-env");
        until("the child to report its environment", || dumped.exists()).await;

        let env = std::fs::read_to_string(&dumped).unwrap();
        assert!(
            env.lines()
                .any(|l| l == format!("CARGO_TARGET_DIR={}", target_dir.display())),
            "the child was not given the off-volume CARGO_TARGET_DIR:\n{env}"
        );
        assert!(
            env.lines()
                .any(|l| l == format!("TMPDIR={}", tmp_dir.display())),
            "the child was not given the off-volume TMPDIR:\n{env}"
        );

        std::fs::remove_dir_all(sup.scratch_root()).ok();
        std::fs::remove_dir_all(&dir).ok();
    }

    /// QA's WOW-workspace-not-in-creds. The agent's working directory was
    /// `data_dir`, and `creds/` is a child of it — so every agent ran with its
    /// cwd set to the PARENT of every node's credential store.
    ///
    /// The assertion is the RELATIONSHIP, not the path. A future edit that
    /// moves the workspace somewhere else is fine; one that puts it back above
    /// the credentials is not, and a literal path comparison would not know
    /// the difference.
    #[tokio::test]
    async fn an_agents_working_directory_does_not_contain_the_credential_store() {
        let (sup, id, dir) = shim_supervisor("ws-not-creds", ENV_DUMP_HARNESS);
        sup.start(id).await.unwrap();
        let dumped = dir.join("child-env");
        until("the child to report its environment", || dumped.exists()).await;

        let env = std::fs::read_to_string(&dumped).unwrap();
        let cwd = env
            .lines()
            .find_map(|l| l.strip_prefix("PWD="))
            .map(std::path::PathBuf::from)
            .expect("the child reported no working directory");
        // Canonicalised, because the shell reports a RESOLVED path and
        // `temp_dir()` on macOS is `/var/...` symlinked to `/private/var/...`.
        // Comparing the two spellings made this assertion unfalsifiable: it
        // passed with the bug deliberately restored, which is the only reason
        // I found it.
        let real =
            |p: &std::path::Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
        let cwd = real(&cwd);
        let creds = real(&sup.cfg.creds_dir());

        assert!(
            !creds.starts_with(&cwd),
            "the agent's cwd {} contains the credential store {} — `ls .` enumerates \
             every node's credentials, and anything the agent writes lands beside them",
            cwd.display(),
            creds.display()
        );
        assert_ne!(
            cwd,
            real(&sup.cfg.data_dir),
            "the data root is not a working copy"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// The startup tail is trimmed by whole lines, so no arithmetic can land
    /// an index inside a character.
    ///
    /// The old buffer drained a `String` at a byte offset computed from
    /// lengths, which panics off a character boundary — the same defect as the
    /// envelope escaper, on the supervisor's stdout reader. QA found it by
    /// grepping the class rather than the instance, which is the right way to
    /// have found the second one.
    #[test]
    fn the_startup_tail_survives_multibyte_output_at_every_alignment() {
        // Each line carries a multi-byte character, and the lines are of every
        // length across a character width, so the old byte cut would have
        // landed inside one. The offset is the variable; the presence of
        // unicode is not.
        for pad in 0..8 {
            let mut tail = StartupTail::default();
            for i in 0..4_000 {
                tail.push(format!("{}\u{2014} line {i} \u{1f600}", "x".repeat(pad)));
            }
            let out = tail.into_string();
            assert!(
                out.len() <= STARTUP_OUTPUT_TAIL + 64,
                "the tail grew to {} bytes at pad {pad}",
                out.len()
            );
            assert!(out.contains('\u{2014}'), "multibyte content was mangled");
            assert!(
                out.contains("line 3999"),
                "the tail must keep the LAST output, which is what says why it died"
            );
        }
    }

    /// One line longer than the whole budget is still kept: an over-long error
    /// banner is the best evidence there is of why a child died, and dropping
    /// it to respect a byte ceiling would discard exactly the thing the buffer
    /// exists for.
    #[test]
    fn a_single_over_long_line_is_kept_rather_than_dropped() {
        let mut tail = StartupTail::default();
        tail.push("\u{2014}".repeat(STARTUP_OUTPUT_TAIL));
        let out = tail.into_string();
        assert!(!out.is_empty(), "the only line was discarded");
        assert!(out.contains('\u{2014}'));
    }

    /// API's measurement, as a property: 1.76G of byte-identical pnpm store
    /// across two agents, in each node's private `$HOME` where nothing shares
    /// or reclaims it. Six agents would have been 5.3G on a 4.6G volume — the
    /// outage again with a different filename.
    ///
    /// The assertion is that every cache the child is told about is under the
    /// PROJECT's directory and none is under the node's own, because "per
    /// node" is exactly what made the same bytes land N times.
    #[tokio::test]
    async fn every_toolchain_cache_is_per_project_and_none_is_in_the_node_home() {
        let (sup, id, dir) = shim_supervisor("tool-caches", ENV_DUMP_HARNESS);
        sup.start(id).await.unwrap();
        let dumped = dir.join("child-env");
        until("the child to report its environment", || dumped.exists()).await;
        let env = std::fs::read_to_string(&dumped).unwrap();

        let node_home = sup.cfg.creds_dir().join(id.to_string());
        for (var, _) in TOOL_CACHES {
            let value = env
                .lines()
                .find_map(|l| l.strip_prefix(&format!("{var}=")))
                .unwrap_or_else(|| panic!("{var} was never given to the child:\n{env}"));
            let path = std::path::Path::new(value);
            assert!(
                path.starts_with(&sup.cfg.data_dir),
                "{var}={value} is not under this project's directory"
            );
            assert!(
                !path.starts_with(&node_home),
                "{var}={value} is inside the node's own HOME, so every agent \
                 downloads its own copy — that is the bug this prevents"
            );
            // The load-bearing one. "Not in $HOME" is not the property; "the
            // same for every agent on this board" is, and a scheme that gives
            // each node its own directory UNDER the project satisfies the
            // first while duplicating exactly as before. A path that varies
            // per node must mention the node, so it must not.
            assert!(
                !value.contains(&id.to_string()),
                "{var}={value} is keyed by the node id, so each agent gets its own copy"
            );
        }

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Two agents on one board share every cache. Stated separately because
    /// the test above passes perfectly well for a scheme that gives each node
    /// its own directory under the project — which would still duplicate.
    #[tokio::test]
    async fn two_agents_on_a_board_share_one_cache_of_each_kind() {
        let (sup, _id, dir) = shim_supervisor("tool-share", ECHO_HARNESS);
        for (_, cache) in TOOL_CACHES {
            let a = sup.project_cache(cache).unwrap();
            let b = sup.project_cache(cache).unwrap();
            assert_eq!(a, b, "{cache} differs between two starts on one project");
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A harness that records the environment it was actually given.
    const ENV_DUMP_HARNESS: &str = r#"#!/bin/sh
dir=$(dirname "$0")
env > "$dir/child-env.tmp"
mv "$dir/child-env.tmp" "$dir/child-env"
echo "{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"s1\"}"
while IFS= read -r line; do
  echo "{\"type\":\"result\",\"subtype\":\"success\",\"session_id\":\"s1\",\"is_error\":false,\"result\":\"ok\"}"
done
"#;

    /// ADVERSARY F015. A child inherited the ENGINE's whole environment, so an
    /// agent could read `WHEEL_ENGINE_SECRET` (the control-plane bearer) and
    /// `WHEEL_VAULT_KEY` (which decrypts every vault in the project) straight
    /// out of its own `/proc/self/environ` — no wire, no token, no uid trick.
    /// Both were demonstrated live: the agent self-granted a wire to a vault
    /// it was never connected to.
    #[tokio::test]
    async fn a_child_is_not_given_the_engines_own_secrets() {
        // Set on the ENGINE's process, which is exactly how the host supplies
        // them in production.
        // Restored on the way out. Environment is PROCESS-global and tests run
        // in parallel: leaving WHEEL_ENGINE_SECRET and WHEEL_VAULT_KEY set
        // would hand every sibling test this fixture's canaries instead of
        // their own. Writing a test to prove secrets do not leak to children,
        // and leaking them to other tests in the process, is not a mistake to
        // make twice.
        let _restore = EnvGuard::set(&[
            ("RUSTUP_HOME", "/opt/rust/rustup"),
            ("WHEEL_ENGINE_SECRET", "engine-bearer-must-not-leak"),
            ("WHEEL_VAULT_KEY", "dmF1bHQta2V5LW11c3Qtbm90LWxlYWs="),
        ]);

        let (sup, id, dir) = shim_supervisor("env-hygiene", ENV_DUMP_HARNESS);
        sup.start(id).await.unwrap();

        let dumped = dir.join("child-env");
        until("the child to report its environment", || dumped.exists()).await;
        let env = std::fs::read_to_string(&dumped).unwrap();

        for secret in [
            "WHEEL_ENGINE_SECRET",
            "WHEEL_VAULT_KEY",
            "WHEEL_HOST_SECRET",
            "WHEEL_PROJECT_ID",
            "WHEEL_ROLE",
            "WHEEL_LISTEN",
        ] {
            assert!(
                !env.contains(secret),
                "{secret} reached an untrusted child:\n{env}"
            );
        }
        // And the values themselves, in case a name is ever spelled anew.
        assert!(!env.contains("engine-bearer-must-not-leak"));
        assert!(!env.contains("dmF1bHQta2V5LW11c3Qtbm90LWxlYWs="));

        // The child must still be able to WORK: an empty environment that
        // cannot find its own binary would pass the assertions above and
        // break every agent on the board.
        assert!(env.contains("PATH="), "the harness needs a PATH:\n{env}");
        assert!(
            env.contains("RUSTUP_HOME="),
            "without RUSTUP_HOME an agent cannot build anything, which is what \
             broke the first Wheel-on-Wheel run:\n{env}"
        );
        assert!(env.contains("WHEEL_NODE="), "the child lost its identity");
        assert!(
            env.contains("WHEEL_TOKEN_FILE="),
            "the child lost its capability token"
        );

        sup.stop(id).await.ok();
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Announces itself only AFTER its first input, then answers normally.
    ///
    /// This is the hypothesis about the real `claude` CLI that PM could not
    /// discriminate from the logs: "the init event never comes" and "it comes
    /// and we drop it" look identical from outside, and a harness that simply
    /// has nothing to say until it is spoken to produces the first without any
    /// bug in the engine.
    const LATE_INIT_HARNESS: &str = r#"#!/bin/sh
dir=$(dirname "$0")
echo run >> "$dir/runs"
session=$(cat "$dir/session" 2>/dev/null || echo s1)
announced=no
while IFS= read -r line; do
  if [ "$announced" = "no" ]; then
    echo "{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"$session\"}"
    announced=yes
  fi
  echo "{\"type\":\"result\",\"subtype\":\"success\",\"session_id\":\"$session\",\"is_error\":false,\"result\":\"ok\"}"
done
"#;

    /// Comes up and never emits `init`: the child is alive, its pipes are
    /// open, and it will answer nothing. This is what production looked like —
    /// `pm` in `starting`, /healthz at 200, three messages queued.
    const SILENT_HARNESS: &str = r#"#!/bin/sh
dir=$(dirname "$0")
echo run >> "$dir/runs"
while IFS= read -r line; do :; done
sleep 300
"#;

    /// Reports turns and cost the way the real harness does: CUMULATIVELY for
    /// the session. Turn one says 1, turn two says 2 — not 1 and 1.
    const ACCOUNTING_HARNESS: &str = r#"#!/bin/sh
dir=$(dirname "$0")
echo run >> "$dir/runs"
session=$(cat "$dir/session" 2>/dev/null || echo s1)
turn=0
echo "{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"$session\"}"
while IFS= read -r line; do
  [ -z "$line" ] && continue
  turn=$((turn + 1))
  cost=$(awk "BEGIN{printf \"%.2f\", $turn * 0.25}")
  echo "{\"type\":\"result\",\"subtype\":\"success\",\"session_id\":\"$session\",\"is_error\":false,\"result\":\"ok\",\"num_turns\":$turn,\"total_cost_usd\":$cost}"
done
"#;

    const ECHO_HARNESS: &str = r#"#!/bin/sh
dir=$(dirname "$0")
echo run >> "$dir/runs"
session=$(cat "$dir/session" 2>/dev/null || echo s1)
resumed=no
while [ $# -gt 0 ]; do
  if [ "$1" = "--resume" ]; then resumed=yes; session=$2; fi
  shift
done
echo "$resumed" >> "$dir/resumes"
echo "{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"$session\"}"
while IFS= read -r line; do
  echo "{\"type\":\"result\",\"subtype\":\"success\",\"session_id\":\"$session\",\"is_error\":false,\"result\":\"ok\"}"
done
"#;

    fn count(path: &std::path::Path) -> usize {
        std::fs::read_to_string(path)
            .map(|s| s.lines().count())
            .unwrap_or(0)
    }

    fn enqueue(sup: &Supervisor, to: Uuid, body: &str) {
        let conn = sup.db.lock().unwrap();
        messages::enqueue(
            &conn,
            wheel_core::MessageSender::User,
            to,
            body.to_string(),
            None,
            None,
        )
        .unwrap();
    }

    fn enqueue_from_endpoint(sup: &Supervisor, to: Uuid, body: &str) {
        let conn = sup.db.lock().unwrap();
        messages::enqueue(
            &conn,
            wheel_core::MessageSender::Node {
                id: Uuid::new_v4(),
                name: "tg".parse().unwrap(),
                node_type: NodeType::Endpoint,
            },
            to,
            body.to_string(),
            None,
            None,
        )
        .unwrap();
    }

    /// The mechanism behind PM's finding, demonstrated without touching the
    /// operator's credentials: a harness that says nothing until it is spoken
    /// to leaves the agent in `starting` for as long as its queue is empty —
    /// which, for an ephemeral agent, is where it LIVES between turns.
    ///
    /// Nothing here is broken in the engine: `starting` is being used to mean
    /// two different things — "we are spawning it" and "it is up but has not
    /// announced itself" — and only the first is transitional. Delivery keeps
    /// working underneath, exactly as PM observed, because `pump_queue` needs a
    /// live process and not a status.
    #[tokio::test]
    async fn a_harness_that_announces_itself_late_leaves_the_agent_looking_transitional() {
        let (sup, id, dir) = shim_supervisor("late-init", LATE_INIT_HARNESS);

        sup.start(id).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(600)).await;
        assert_eq!(
            status_of(&sup, id),
            AgentStatus::Starting,
            "with nothing queued and no init yet, the agent reads as transitional"
        );

        // Speak to it, and everything resolves — which is why this is invisible
        // on a busy board and permanent on a quiet one.
        enqueue(&sup, id, "say something");
        sup.deliver(id).await.unwrap();
        until("the agent to settle once it has been spoken to", || {
            matches!(status_of(&sup, id), AgentStatus::Idle)
        })
        .await;

        sup.stop(id).await.unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    /// turns=0 and usd=0.0 for every agent, all night: the harness hands us
    /// `num_turns` and `total_cost_usd` on every result and the parser threw
    /// both away.
    ///
    /// The arithmetic is asserted here rather than through a child process,
    /// because the risky part is not whether spend is WIRED — it is whether it
    /// is COUNTED. Both figures are cumulative for the session, so adding the
    /// reports directly makes a board's total a triangular series: three turns
    /// would read as six, and the money would be wrong the same way.
    #[test]
    fn cumulative_session_figures_become_deltas_not_a_triangular_series() {
        // One session reporting 1, 2, 3 turns at 0.25, 0.50, 0.75 cumulative.
        let mut turns = 0u64;
        let mut usd = 0.0f64;
        for (reported_t, reported_u) in [(1u64, 0.25f64), (2, 0.50), (3, 0.75)] {
            let (dt, du) = spend_delta(Some(reported_t), Some(reported_u), turns, usd);
            turns += dt;
            usd += du;
        }
        assert_eq!(turns, 3, "three turns is three, not 1+2+3");
        assert!((usd - 0.75).abs() < 1e-9, "0.75 spent, not 1.50; got {usd}");

        // A harness that reports nothing still counts the turn we just watched
        // complete — a `result` IS a completed turn.
        assert_eq!(spend_delta(None, None, 4, 1.0), (1, 0.0));

        // And one that resets its counter mid-session contributes nothing
        // rather than refunding the agent's budget.
        assert_eq!(spend_delta(Some(1), Some(0.10), 9, 2.50), (0, 0.0));
    }

    /// The wiring, end to end: a real child reports usage and it reaches the
    /// board. Deliberately asserts "counted at all" rather than an exact total
    /// — the shell shim's turn count is its own, and pinning it here would be
    /// testing the fixture. The exact arithmetic is pinned above.
    #[tokio::test]
    async fn usage_reported_by_a_child_reaches_the_board() {
        let (sup, id, dir) = shim_supervisor("accounting", ACCOUNTING_HARNESS);

        enqueue(&sup, id, "one");
        sup.start(id).await.unwrap();
        sup.deliver(id).await.unwrap();

        until("the turn to be counted", || spend_of(&sup, id).0 >= 1).await;
        let (turns, usd) = spend_of(&sup, id);
        assert!(turns >= 1, "turns must leave 0 once a turn completes");
        assert!(
            usd > 0.0,
            "cost must leave 0.0 once a turn completes, got {usd}"
        );

        sup.stop(id).await.unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    /// §3e's `budget` was a config field nothing consulted, so an agent in a
    /// loop burned tokens with nothing to stop it. It is the same root as the
    /// discarded usage: a ceiling cannot be enforced against a total nobody
    /// counts.
    #[tokio::test]
    async fn an_agent_that_reaches_its_turn_budget_is_stopped_with_the_reason() {
        let (sup, id, dir) = shim_supervisor_cfg("budgeted", ACCOUNTING_HARNESS, |c| {
            c.budget = Some(wheel_core::Budget {
                max_turns: Some(1),
                max_usd: None,
            });
        });

        enqueue(&sup, id, "the only turn this agent may take");
        sup.start(id).await.unwrap();
        sup.deliver(id).await.unwrap();

        until("the agent to be stopped by its budget", || {
            matches!(status_of(&sup, id), AgentStatus::BudgetExhausted)
        })
        .await;

        let state = {
            let conn = sup.db.lock().unwrap();
            board::agent_state(&conn, id).unwrap_or_default()
        };
        let reason = state.last_error.unwrap_or_default();
        assert!(
            reason.contains("budget") && reason.contains("max_turns"),
            "the operator needs to know which ceiling stopped it and how to raise it, got {reason:?}"
        );
        assert_eq!(
            count(&dir.join("runs")),
            1,
            "a budget stop must not respawn the agent it just stopped"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// PM's discriminator, measured on production: `pm` is the ONLY agent on
    /// the board with `ephemeral_context = true`, and it is the only one stuck
    /// in `starting`. Its status went transitional TWO SECONDS AFTER a turn
    /// completed successfully — a restart following turn completion, which is
    /// what clearing an ephemeral context does. The other five agents, same
    /// engine, same deploy, settle normally.
    ///
    /// The existing ephemeral test asserts a second PROCESS spawns and that it
    /// does not resume. Neither of those notices an agent that respawns and
    /// then never leaves `starting`, which is where the operator's own agent
    /// LIVES between turns.
    #[tokio::test]
    async fn an_ephemeral_agent_settles_after_its_context_is_cleared() {
        let (sup, id, dir) = shim_supervisor_cfg("ephemeral-settles", ECHO_HARNESS, |c| {
            c.ephemeral_context = true;
        });

        enqueue(&sup, id, "one turn, then throw the context away");
        sup.start(id).await.unwrap();
        sup.deliver(id).await.unwrap();

        // Wait for the DESTINATION, not for "no longer transitional". The
        // agent passes THROUGH `idle` on its way — start sets `starting`, the
        // harness's init sets `idle`, writing the turn sets `running`, and only
        // then does the ephemeral clear park it. "Not starting and not running"
        // is TRUE at idle, so this returned early whenever the poll landed in
        // that window and then asserted `parked` against an agent that was
        // merely idle. Green most of the time, red about one run in three.
        //
        // Waiting on the state the test is actually about removes the window
        // entirely: if it never parks, `until` times out and says so, which is
        // a real failure rather than a coin toss.
        until("the ephemeral agent to park after its turn", || {
            matches!(status_of(&sup, id), AgentStatus::Parked)
        })
        .await;

        assert_eq!(
            status_of(&sup, id),
            AgentStatus::Parked,
            "an ephemeral agent must settle into a state idle-parking recognises. It used to sit \
             in `starting` for ever, because the respawn wrote nothing to the fresh child and the \
             harness announces itself only when it processes a turn — so no init ever arrived, and \
             the only Starting -> Idle transition is the Init arm."
        );
        assert_eq!(
            count(&dir.join("runs")),
            1,
            "and it holds no process between turns: idle parking keys on the settled state, so an \
             agent that never reached one kept a live harness 24/7 (§3c#14)"
        );

        sup.stop(id).await.unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The half that parking could plausibly break: work queued BEHIND the turn
    /// must not wait for some later message to trigger it.
    ///
    /// This engine has already had that bug from the other direction — a
    /// message with no event left to deliver it — so parking without this
    /// assertion would be trading one silent queue for another.
    #[tokio::test]
    async fn an_ephemeral_agent_that_parks_still_drains_what_queued_behind_the_turn() {
        let (sup, id, dir) = shim_supervisor_cfg("ephemeral-drains", ECHO_HARNESS, |c| {
            c.ephemeral_context = true;
        });

        enqueue(&sup, id, "first");
        sup.start(id).await.unwrap();
        sup.deliver(id).await.unwrap();
        enqueue(&sup, id, "queued behind the first turn");

        until(
            "both messages to be consumed without a new message arriving",
            || {
                let conn = sup.db.lock().unwrap();
                !messages::has_queued(&conn, id).unwrap_or(true)
            },
        )
        .await;

        sup.stop(id).await.unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    /// ADVERSARY 041 / QA's PROGRESS-deadline-outcome: an agent with WORK
    /// QUEUED must not sit transitional for ever.
    ///
    /// The three assertions are QA's, and each exists because a weaker fix
    /// passes without them: the status afterwards is an ANSWER, it carries a
    /// readable reason, and NO SECOND PROCESS was spawned. That last one is
    /// what separates a fix from a respawn loop — at any single instant a
    /// respawn loop is indistinguishable from an agent that is starting.
    #[tokio::test]
    async fn an_agent_wedged_in_starting_with_work_queued_settles_with_a_reason() {
        let (sup, id, dir) = shim_supervisor_full("wedged-start", SILENT_HARNESS, |_| {}, 2);

        enqueue_from_endpoint(&sup, id, "a message that cannot be delivered");
        sup.start(id).await.unwrap();

        // Wait for the child to actually exist before judging how many there
        // are. Asserting the count straight after `start` races the shim's own
        // exec: under a loaded suite it read 0 processes and called that a
        // violation of "never a second process", which is the opposite of what
        // it measures. The deadline may well fire while this is waiting — that
        // is fine, and the point: it does not kill the child.
        until("the child to spawn at all", || {
            count(&dir.join("runs")) >= 1
        })
        .await;

        until("the wedged agent to settle into an answer", || {
            !matches!(status_of(&sup, id), AgentStatus::Starting)
        })
        .await;

        let state = {
            let conn = sup.db.lock().unwrap();
            board::agent_state(&conn, id).unwrap_or_default()
        };
        assert_eq!(
            state.status,
            AgentStatus::Error,
            "a deadline that returns the agent to a transitional state has made the hang \
             periodic rather than resolved it"
        );
        assert!(
            state.last_error.as_deref().unwrap_or("").trim().len() > 20,
            "an operator watching the board needs to know WHY; got {:?}",
            state.last_error
        );
        assert_eq!(
            count(&dir.join("runs")),
            1,
            "the deadline must not replace the child: one agent node, one process (§3c#13). \
             Killing and respawning would satisfy `resolves within 60s` while making the hang \
             periodic, which is worse than the bug."
        );

        sup.stop(id).await.unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The other half of the predicate, and the reason it is not "time in
    /// `starting`": an agent with an EMPTY queue that is slow to come up is
    /// WAITING, not wedged. Production's `pm` sat in `starting` 52 minutes with
    /// nothing queued and was healthy. A flat deadline would have killed it
    /// every minute forever.
    #[tokio::test]
    async fn an_agent_with_nothing_queued_is_left_alone_however_long_it_takes() {
        let (sup, id, dir) = shim_supervisor_full("slow-but-idle", SILENT_HARNESS, |_| {}, 1);

        sup.start(id).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;

        assert_eq!(
            status_of(&sup, id),
            AgentStatus::Starting,
            "nothing was queued, so there was nothing to be late for; the deadline must \
             decline to judge rather than invent a failure"
        );

        sup.stop(id).await.unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    /// ADVERSARY 040: the `catch_unwind` quarantine belt was exercised by ZERO
    /// tests, so `panic = "unwind"` was load-bearing in Cargo.toml with nothing
    /// proving it. ADVERSARY filed this against their own earlier verdict,
    /// having credited the belt as verified in 035 and then run the tests.
    ///
    /// The belt exists because a stored body that panicked the encoder is
    /// REPLAYED at every start, so one bad message took a whole board down
    /// through repeated reboots. Catching the panic is only half; the message
    /// must be set aside so the next start does not hit it again.
    #[tokio::test]
    async fn a_body_that_panics_the_encoder_is_quarantined_and_the_agent_carries_on() {
        let (sup, id, dir) = shim_supervisor_driver("poison", ECHO_HARNESS, |program| {
            Arc::new(PoisonDriver { program })
        });

        let poison = {
            let conn = sup.db.lock().unwrap();
            messages::enqueue(
                &conn,
                wheel_core::MessageSender::User,
                id,
                format!("a body containing {POISON}"),
                None,
                None,
            )
            .unwrap()
        };
        enqueue(&sup, id, "an ordinary message behind the poison");

        sup.start(id).await.unwrap();
        sup.deliver(id).await.unwrap();

        until("the queue to drain past the poison", || {
            let conn = sup.db.lock().unwrap();
            !messages::has_queued(&conn, id).unwrap_or(true)
        })
        .await;

        let states: Vec<(String, Option<String>)> = {
            let conn = sup.db.lock().unwrap();
            let mut stmt = conn
                .prepare("SELECT id, state, last_error FROM messages WHERE to_id = ?1")
                .unwrap();
            stmt.query_map([id.to_string()], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1).ok().map(|s| {
                        format!(
                            "{s}|{}",
                            r.get::<_, Option<String>>(2)
                                .ok()
                                .flatten()
                                .unwrap_or_default()
                        )
                    }),
                ))
            })
            .unwrap()
            .flatten()
            .collect()
        };

        let poisoned = states
            .iter()
            .find(|(mid, _)| *mid == poison.id.to_string())
            .map(|(_, s)| s.clone().unwrap_or_default())
            .unwrap_or_default();
        assert!(
            poisoned.starts_with("undeliverable"),
            "the panicking body must be set aside, not retried for ever; got {poisoned:?}"
        );
        assert!(
            poisoned.contains('|') && poisoned.split('|').nth(1).is_some_and(|r| !r.is_empty()),
            "and it must say WHY, or the operator cannot tell which message was dropped"
        );

        // The half that makes it a belt rather than a bin: the agent kept going.
        assert!(
            !matches!(status_of(&sup, id), AgentStatus::Error),
            "one unencodable message must not take the agent down with it"
        );

        sup.stop(id).await.unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    /// QA's S1, reproduced: the message arrives while the agent is STARTING.
    ///
    /// `pump_queue` runs in exactly two places — when something enqueues, and
    /// when a turn ends. An agent that reaches `idle` by starting up has ended
    /// no turn, so anything queued while it was coming up is never picked up.
    /// Nothing retries: the 60s promotion in `next_for_delivery` can only apply
    /// on a pump that never happens, which is why QA measured a message still
    /// `queued` at 75s.
    ///
    /// This is the shape production had — three messages behind an agent in
    /// `starting` — and it is why fixing ingress to call `deliver` was
    /// necessary but not sufficient: `deliver` pumps once, immediately, into an
    /// agent with no process to write to yet.
    #[tokio::test]
    async fn a_message_queued_while_the_agent_starts_is_delivered_once_it_is_up() {
        let (sup, id, dir) = shim_supervisor("queued-during-start", ECHO_HARNESS);

        // Deliberately NOT awaiting idle first: the whole bug is the window
        // between "start was asked for" and "the child can be written to".
        enqueue_from_endpoint(&sup, id, "arrived while the agent was starting");
        sup.start(id).await.unwrap();

        until("the message queued during startup to be delivered", || {
            let conn = sup.db.lock().unwrap();
            !messages::has_queued(&conn, id).unwrap_or(true)
        })
        .await;

        sup.stop(id).await.unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    /// QA's S1, through the REAL ingress handler rather than past it.
    ///
    /// The test below proves the supervisor delivers an endpoint message to a
    /// warm agent. That passed while production did not, which means the fault
    /// was never in `deliver` — it is in what ingress does with it. So this
    /// drives `ingress::deliver` itself, with a live child on the other end,
    /// and asserts the bytes actually leave the queue.
    ///
    /// This is the test I first said could not be written here. It can: the
    /// shim harness gives a real child, and `AppState` is constructible from
    /// the same pieces `serve` uses.
    #[tokio::test]
    async fn an_ingress_hit_drains_to_an_agent_that_is_already_warm() {
        use crate::api::ingress::{deliver as ingress_deliver, MatchedEndpoint};
        use axum::http::{HeaderMap, Method};

        let (sup, id, dir) = shim_supervisor("warm-ingress-route", ECHO_HARNESS);

        // The endpoint node, wired `send` into the agent — the shape PM
        // created on the wheel-dev board for the Telegram tile.
        let endpoint = {
            let conn = sup.db.lock().unwrap();
            let ep = wheel_core::Node::new(
                Uuid::new_v4(),
                "tg".parse().unwrap(),
                wheel_core::Position::default(),
                wheel_core::NodeConfig::Endpoint(wheel_core::EndpointConfig {
                    method: wheel_core::HttpMethod::Post,
                    path: "/telegram".into(),
                    response_mode: wheel_core::ResponseMode::Ack,
                    auth: wheel_core::EndpointAuth::None,
                }),
            );
            board::create(&conn, &ep).unwrap();
            board::add_wire(&conn, ep.id, id, wheel_core::WireType::Send, None).unwrap();
            ep
        };

        sup.start(id).await.unwrap();
        until("the agent to be warm and idle", || {
            status_of(&sup, id) == AgentStatus::Idle
        })
        .await;

        let state = crate::api::AppState {
            cfg: sup.cfg.clone(),
            supervisor: sup.clone(),
            db: sup.db.clone(),
            events: sup.events().clone(),
            ingress_rate: Arc::new(crate::api::ingress::RateLimiter::default()),
            logins: Arc::new(crate::oauth::LoginSessions::default()),
        };

        let matched = MatchedEndpoint {
            id: endpoint.id,
            name: endpoint.name.clone(),
            config: match &endpoint.config {
                wheel_core::NodeConfig::Endpoint(c) => c.clone(),
                _ => unreachable!(),
            },
        };

        ingress_deliver(
            &state,
            &matched,
            &Method::POST,
            "/telegram",
            &HeaderMap::new(),
            b"{\"message\":\"hello from telegram\"}",
        );

        until("the first ingress message to leave the queue", || {
            let conn = sup.db.lock().unwrap();
            !messages::has_queued(&conn, id).unwrap_or(true)
        })
        .await;

        // QA's exact interleaving: ingress, user, user, ingress. They ran this
        // order deliberately, because a suite that happens to send user-first
        // would be equally explained by "the second message never drains,
        // whatever produced it" — which points at the wrong file.
        enqueue(&sup, id, "user one");
        sup.deliver(id).await.unwrap();
        enqueue(&sup, id, "user two");
        sup.deliver(id).await.unwrap();
        ingress_deliver(
            &state,
            &matched,
            &Method::POST,
            "/telegram",
            &HeaderMap::new(),
            b"{\"message\":\"the second telegram hit\"}",
        );

        until("every message to leave the queue", || {
            let conn = sup.db.lock().unwrap();
            !messages::has_queued(&conn, id).unwrap_or(true)
        })
        .await;

        sup.stop(id).await.unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    /// QA's S1, found by PROGRESS-message-reaches-consumed/endpoint-ingress:
    /// an ingress message to an agent that is ALREADY RUNNING never leaves
    /// `queued`, while user messages to the same agent in the same session are
    /// consumed instantly.
    ///
    /// Every delivery test above enqueues as `MessageSender::User`, so the one
    /// axis this bug lives on — who the message is FROM — was the one axis
    /// nothing varied. The parked case works because the queue drains on
    /// start; this is the warm case, where there is no start transition to do
    /// the draining.
    #[tokio::test]
    async fn an_endpoint_message_reaches_a_warm_agent_not_only_a_parked_one() {
        let (sup, id, dir) = shim_supervisor("warm-ingress", ECHO_HARNESS);

        sup.start(id).await.unwrap();
        until("the agent to be warm and idle", || {
            status_of(&sup, id) == AgentStatus::Idle
        })
        .await;

        enqueue_from_endpoint(&sup, id, "from the telegram tile");
        sup.deliver(id).await.unwrap();

        until("the endpoint message to leave the queue", || {
            let conn = sup.db.lock().unwrap();
            !messages::has_queued(&conn, id).unwrap_or(true)
        })
        .await;

        sup.stop(id).await.unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    /// `ephemeral_context`: the turn ends, the session is thrown away, and the
    /// next message runs in a NEW one. If the session id survived, the context
    /// the operator asked to discard survived with it.
    #[tokio::test]
    async fn an_ephemeral_agent_gets_a_new_session_after_every_turn() {
        let (sup, id, dir) = shim_supervisor_cfg("ephemeral", ECHO_HARNESS, |c| {
            c.ephemeral_context = true;
        });

        enqueue(&sup, id, "first");
        sup.start(id).await.unwrap();
        sup.deliver(id).await.unwrap();

        // The turn ends and the agent PARKS. It used to respawn here, into a
        // queue the turn had just emptied; see `clear_context` for what that
        // cost.
        until("the ephemeral agent to park after its turn", || {
            matches!(status_of(&sup, id), AgentStatus::Parked)
        })
        .await;
        assert_eq!(
            count(&dir.join("runs")),
            1,
            "parking must not spawn a replacement child for a queue that is empty"
        );

        // The next message resumes it — into a FRESH session, which is the
        // property this test has always been about.
        enqueue(&sup, id, "second");
        sup.deliver(id).await.unwrap();
        until("the next message to start a new child", || {
            count(&dir.join("runs")) == 2
        })
        .await;

        let resumes = std::fs::read_to_string(dir.join("resumes")).unwrap();
        assert!(
            resumes.lines().all(|l| l == "no"),
            "an ephemeral agent must not --resume the context it just discarded, got: {resumes:?}"
        );
        sup.stop(id).await.unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The opposite, so the test above is not just proving that restarts
    /// happen: a normal agent keeps ONE process and one session across turns.
    #[tokio::test]
    async fn a_normal_agent_keeps_its_session_across_turns() {
        let (sup, id, dir) = shim_supervisor("persistent", ECHO_HARNESS);

        enqueue(&sup, id, "first");
        sup.start(id).await.unwrap();
        sup.deliver(id).await.unwrap();
        until("the first turn to complete", || {
            status_of(&sup, id) == AgentStatus::Idle
        })
        .await;

        enqueue(&sup, id, "second");
        sup.deliver(id).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert_eq!(
            count(&dir.join("runs")),
            1,
            "a non-ephemeral agent must keep one process across turns"
        );
        sup.stop(id).await.unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    /// `run_on_startup` comes up PARKED, not running (§2). A board of twenty
    /// agents must not cost twenty idle processes at boot.
    #[tokio::test]
    async fn run_on_startup_parks_rather_than_spawning() {
        let (sup, id, dir) = shim_supervisor_cfg("parky", ECHO_HARNESS, |c| {
            c.run_on_startup = true;
        });

        sup.start_configured_agents().await;
        assert_eq!(status_of(&sup, id), AgentStatus::Parked);
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert_eq!(
            count(&dir.join("runs")),
            0,
            "parking must not spawn a process"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// ...and a parked agent wakes when something is actually addressed to it.
    /// Without this the frugality above would just be a way to never run.
    #[tokio::test]
    async fn a_parked_agent_resumes_when_a_message_arrives() {
        let (sup, id, dir) = shim_supervisor_cfg("wakeup", ECHO_HARNESS, |c| {
            c.run_on_startup = true;
        });
        sup.start_configured_agents().await;
        assert_eq!(status_of(&sup, id), AgentStatus::Parked);

        enqueue(&sup, id, "wake up");
        sup.deliver(id).await.unwrap();
        until("the parked agent to spawn on demand", || {
            count(&dir.join("runs")) == 1
        })
        .await;
        sup.stop(id).await.unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Queued work from a previous run is picked up at boot, so a message that
    /// arrived while the engine was down is not stranded behind a parked agent.
    #[tokio::test]
    async fn boot_resumes_a_parked_agent_that_already_has_queued_work() {
        let (sup, id, dir) = shim_supervisor_cfg("bootwork", ECHO_HARNESS, |c| {
            c.run_on_startup = true;
        });
        enqueue(&sup, id, "left over from last time");

        sup.start_configured_agents().await;
        until("boot to resume the agent holding queued work", || {
            count(&dir.join("runs")) == 1
        })
        .await;
        sup.stop(id).await.unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    /// PM repro'd this against the REAL `claude` CLI: no credentials, and the
    /// agent went to `error` with the queued message gone. The CLI announces
    /// "Not logged in · Please run /login" on **stdout** and exits non-zero
    /// with no `result` event, and the classifier was only ever shown stderr.
    ///
    /// The exact bytes the real CLI produces, on the stream it really uses.
    #[tokio::test]
    async fn the_real_cli_logged_out_banner_on_stdout_means_needs_auth() {
        let (sup, id, dir) = shim_supervisor(
            "loggedout",
            "#!/bin/sh\necho 'Not logged in · Please run /login'\nexit 1\n",
        );
        enqueue(&sup, id, "please do the thing");

        sup.start(id).await.unwrap();
        sup.deliver(id).await.unwrap();

        until("the agent to report needs_auth", || {
            status_of(&sup, id) == AgentStatus::NeedsAuth
        })
        .await;

        // ...and the operator's message is still there to be delivered once
        // they authenticate. Consuming it would lose work to a fixable setup
        // problem.
        let queued = {
            let conn = sup.db.lock().unwrap();
            messages::has_queued(&conn, id).unwrap()
        };
        assert!(
            queued,
            "the queued message must survive an auth failure, not be consumed"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The other half of the same fix: including stdout in the diagnosis must
    /// not make an ordinary session's chatter look like a failure.
    #[tokio::test]
    async fn a_chatty_session_that_exits_cleanly_is_not_an_error() {
        let (sup, id, dir) = shim_supervisor(
            "chatty",
            "#!/bin/sh\n\
             echo '{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"s9\"}'\n\
             echo 'something that looks alarming but is just output'\n\
             exit 0\n",
        );
        sup.start(id).await.unwrap();
        until("the agent to settle", || {
            matches!(
                status_of(&sup, id),
                AgentStatus::Stopped | AgentStatus::Error
            )
        })
        .await;
        assert_eq!(
            status_of(&sup, id),
            AgentStatus::Stopped,
            "a child that initialised did not FAIL to start"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// API repro'd this twice in production: agent `error`, queued_messages 0,
    /// last_error the BARE harness string. That bare string was the tell — no
    /// startup-failure branch produces it, so the auth failure was arriving as
    /// an ordinary `result` with is_error, and the turn handler was consuming
    /// the operator's message as poison.
    ///
    /// An environmental failure is not the message's fault.
    #[tokio::test]
    async fn an_auth_failure_reported_as_a_turn_result_requeues_the_message() {
        let (sup, id, dir) = shim_supervisor(
            "authresult",
            "#!/bin/sh\n\
             echo '{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"s1\"}'\n\
             while IFS= read -r line; do\n\
               echo '{\"type\":\"result\",\"subtype\":\"error\",\"session_id\":\"s1\",\"is_error\":true,\"result\":\"Not logged in · Please run /login\"}'\n\
             done\n",
        );
        enqueue(&sup, id, "work the operator does not want to lose");

        sup.start(id).await.unwrap();
        sup.deliver(id).await.unwrap();

        until("the agent to report needs_auth", || {
            status_of(&sup, id) == AgentStatus::NeedsAuth
        })
        .await;

        let queued = {
            let conn = sup.db.lock().unwrap();
            messages::has_queued(&conn, id).unwrap()
        };
        assert!(
            queued,
            "an auth failure must requeue the message, not consume it as poison"
        );
        sup.stop(id).await.unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The other side of that judgement: a REAL task error is still poison and
    /// must be consumed exactly once, or a failing message loops forever.
    #[tokio::test]
    async fn a_genuine_task_error_is_still_consumed_once() {
        let (sup, id, dir) = shim_supervisor(
            "poison",
            "#!/bin/sh\n\
             echo '{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"s1\"}'\n\
             while IFS= read -r line; do\n\
               echo '{\"type\":\"result\",\"subtype\":\"error\",\"session_id\":\"s1\",\"is_error\":true,\"result\":\"tool call failed: no such file\"}'\n\
             done\n",
        );
        enqueue(&sup, id, "a message that genuinely fails");

        sup.start(id).await.unwrap();
        sup.deliver(id).await.unwrap();

        until("the agent to report the error", || {
            status_of(&sup, id) == AgentStatus::Error
        })
        .await;

        let queued = {
            let conn = sup.db.lock().unwrap();
            messages::has_queued(&conn, id).unwrap()
        };
        assert!(
            !queued,
            "a genuine task error must be consumed, or it loops forever"
        );
        sup.stop(id).await.unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Answers each message after the number of seconds in `turn_secs` beside it (default 1).
    /// Writes its own pid to `pid`, so a test can end it from outside the supervisor entirely --
    /// the shape of a cgroup-wide kill, which reaches the agent without going through
    /// `Supervisor::shutdown`'s own `signal_group`/`kill` path at all.
    const SLOW_TURN_HARNESS: &str = r#"#!/bin/sh
dir=$(dirname "$0")
echo run >> "$dir/runs"
echo $$ > "$dir/pid"
echo '{"type":"system","subtype":"init","session_id":"s1"}'
while IFS= read -r line; do
  sleep "$(cat "$dir/turn_secs" 2>/dev/null || echo 1)"
  echo '{"type":"result","subtype":"success","session_id":"s1","is_error":false,"result":"ok"}'
done
"#;

    fn send_user(sup: &Supervisor, to: Uuid, body: &str) -> Uuid {
        let conn = sup.db.lock().unwrap();
        messages::enqueue(
            &conn,
            wheel_core::MessageSender::User,
            to,
            body.to_string(),
            None,
            None,
        )
        .unwrap()
        .id
    }

    fn state_and_error(sup: &Supervisor, mid: Uuid) -> (MessageState, Option<String>) {
        let conn = sup.db.lock().unwrap();
        let m = messages::get(&conn, mid).unwrap().unwrap();
        (m.state, m.last_error)
    }

    async fn deliver_one(sup: &Arc<Supervisor>, id: Uuid, body: &str) -> Uuid {
        sup.start(id).await.unwrap();
        until("the agent to come up", || {
            !matches!(status_of(sup, id), AgentStatus::Starting)
        })
        .await;
        let mid = send_user(sup, id, body);
        sup.deliver(id).await.unwrap();
        until("the message to be in flight", || {
            state_and_error(sup, mid).0 == MessageState::Delivered
        })
        .await;
        mid
    }

    /// Review round 1, finding 5: shutdown took the slot out from under a turn, so the result never
    /// landed and the message sat `delivered` for ever. A turn that can finish inside the drain must
    /// be consumed as any other.
    #[tokio::test]
    async fn shutdown_lets_a_turn_in_flight_finish() {
        let (sup, id, dir) = shim_supervisor("drain-finishes", SLOW_TURN_HARNESS);
        std::fs::write(dir.join("turn_secs"), "1").unwrap();
        let mid = deliver_one(&sup, id, "finish me").await;

        sup.shutdown().await;

        assert_eq!(state_and_error(&sup, mid), (MessageState::Consumed, None));
    }

    /// The other half of finding 5, under the operator's ruling: a turn still running when the drain
    /// runs out is consumed with a reason, never requeued. Replaying a turn that was killed mid-flight
    /// can commit or push twice.
    #[tokio::test]
    async fn a_turn_that_outlasts_the_drain_is_consumed_as_interrupted_and_never_rerun() {
        let (sup, id, dir) = shim_supervisor("drain-expires", SLOW_TURN_HARNESS);
        std::fs::write(dir.join("turn_secs"), "3600").unwrap();
        sup.shutdown_drain_ms.store(300, Ordering::SeqCst);
        let mid = deliver_one(&sup, id, "never finishes").await;

        sup.shutdown().await;

        assert_eq!(
            state_and_error(&sup, mid),
            (
                MessageState::Consumed,
                Some(INTERRUPTED_BY_SHUTDOWN.to_string())
            )
        );
        let conn = sup.db.lock().unwrap();
        assert!(
            !messages::has_queued(&conn, id).unwrap(),
            "the interrupted turn was put back to run again"
        );
    }

    /// Finding 6: once shutdown has begun, nothing may start again, whether asked directly or
    /// resumed by a message arriving for a parked agent.
    #[tokio::test]
    async fn nothing_starts_once_shutdown_has_begun() {
        let (sup, id, dir) = shim_supervisor("closing", ECHO_HARNESS);
        sup.shutdown().await;

        assert!(
            sup.start(id).await.is_err(),
            "an agent started after shutdown"
        );
        sup.set_status(id, AgentStatus::Parked, None);
        send_user(&sup, id, "wake up");
        assert!(sup.deliver(id).await.is_err());
        assert_eq!(runs(&dir), 0, "a process was spawned after shutdown");
    }

    /// Review round 2, finding 1, reproduced live by the security reviewer against
    /// `KillMode=control-group`: that setting SIGTERMs the agent process at the same instant as
    /// wheeld, so the agent can die well before `shutdown`'s own drain-wait even finishes -- its
    /// death is caught by `reap`, not by `shutdown`'s own signal/kill loop, and `reap` must still
    /// refuse to replay the turn. Before this fix `reap` always requeued, so the message came back
    /// `queued` and was redelivered (re-running the turn) the next time the agent started.
    #[tokio::test]
    async fn a_process_killed_out_of_band_during_the_drain_is_interrupted_not_replayed() {
        let (sup, id, dir) = shim_supervisor("killed-during-drain", SLOW_TURN_HARNESS);
        std::fs::write(dir.join("turn_secs"), "3600").unwrap();
        let mid = deliver_one(&sup, id, "never finishes").await;

        // Long enough that this test's own SIGKILL, not `shutdown`'s SHUTDOWN_GRACE-then-kill
        // sequence, is what ends the process.
        sup.shutdown_drain_ms.store(5000, Ordering::SeqCst);
        let shutting_down = tokio::spawn({
            let sup = sup.clone();
            async move { sup.shutdown().await }
        });

        until("shutdown to set the closing flag", || {
            sup.closing.load(Ordering::SeqCst)
        })
        .await;
        // Past the moment `shutdown` has read `in_flight` for its own drain check, so the kill
        // below is unambiguously the thing that ends the process, not a race with it.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let pid: i32 = until_present(&dir.join("pid"))
            .await
            .trim()
            .parse()
            .expect("the harness wrote a pid");
        // The whole process GROUP, not just the leader: the shell forks a real child to run
        // `sleep`, which inherits the same stdout pipe fd, and killing only the leader leaves that
        // child holding the write end open forever, so the reader never sees EOF. A cgroup-wide
        // kill (`KillMode=control-group`) does not have that gap -- it reaches every process in
        // the tree at once -- and `-pid` is the closest a test gets to reproducing that without a
        // real cgroup.
        unsafe { libc::kill(-pid, libc::SIGKILL) };

        until("the interrupted message to be consumed", || {
            state_and_error(&sup, mid).0 == MessageState::Consumed
        })
        .await;
        assert_eq!(
            state_and_error(&sup, mid),
            (
                MessageState::Consumed,
                Some(INTERRUPTED_BY_SHUTDOWN.to_string())
            ),
            "the turn was replayed instead of being marked interrupted"
        );

        shutting_down.await.expect("shutdown itself must not panic");

        // "Not redelivered after a restart": consumed, not queued, so the ordinary
        // resume-on-message path finds nothing waiting for this agent.
        let conn = sup.db.lock().unwrap();
        assert!(
            !messages::has_queued(&conn, id).unwrap(),
            "the interrupted message was put back in the queue"
        );
    }

    /// `POST /v1/agents/:id/interrupt` (§3c#12). Distinguishes it from `stop` on the two axes that
    /// matter: the turn is consumed as interrupted rather than left to be replayed, and the session
    /// survives -- the next message resumes it instead of starting over.
    #[tokio::test]
    async fn interrupt_cancels_an_in_flight_turn_and_keeps_the_session() {
        let (sup, id, dir) = shim_supervisor("interrupt-mid-turn", SLOW_TURN_HARNESS);
        std::fs::write(dir.join("turn_secs"), "3600").unwrap();
        let mid = deliver_one(&sup, id, "never finishes on its own").await;
        let session_before = {
            let conn = sup.db.lock().unwrap();
            board::agent_state(&conn, id).unwrap_or_default().session_id
        };
        assert_eq!(
            session_before.as_deref(),
            Some("s1"),
            "the fake harness always inits as s1"
        );

        let status = sup.interrupt(id).await.unwrap();

        assert_eq!(
            status,
            AgentStatus::Parked,
            "interrupt keeps the session, so it parks"
        );
        assert_eq!(
            state_and_error(&sup, mid),
            (
                MessageState::Consumed,
                Some(INTERRUPTED_BY_USER.to_string())
            ),
            "the cancelled turn must be consumed as interrupted, never left to be replayed"
        );
        let conn = sup.db.lock().unwrap();
        assert!(
            !messages::has_queued(&conn, id).unwrap(),
            "the interrupted message must not go back on the queue"
        );
        assert_eq!(
            board::agent_state(&conn, id)
                .unwrap_or_default()
                .session_id
                .as_deref(),
            Some("s1"),
            "the session id must survive an interrupt -- it is what makes this NOT a stop"
        );
    }

    /// A message queued behind the cancelled turn -- the steering message the caller presumably
    /// wanted delivered -- must not be stranded behind a parked agent nobody wakes up again.
    #[tokio::test]
    async fn a_message_queued_during_interrupt_is_delivered_on_the_resumed_session() {
        let (sup, id, dir) = shim_supervisor("interrupt-then-steer", SLOW_TURN_HARNESS);
        std::fs::write(dir.join("turn_secs"), "3600").unwrap();
        deliver_one(&sup, id, "long turn").await;
        let steer = send_user(&sup, id, "stop and do this instead");

        std::fs::write(dir.join("turn_secs"), "0").unwrap();
        sup.interrupt(id).await.unwrap();

        until("the steering message to be answered", || {
            state_and_error(&sup, steer).0 == MessageState::Consumed
        })
        .await;
        assert_eq!(
            state_and_error(&sup, steer),
            (MessageState::Consumed, None),
            "the queued message must run cleanly on the resumed child, not inherit the interrupt's error"
        );
        assert_eq!(
            runs(&dir),
            2,
            "the resumed turn must be a fresh process, not the killed one"
        );
    }

    /// Interrupting an agent that is not in the middle of anything must not park a perfectly idle
    /// process -- that would make this route a second `stop` wearing the wrong name.
    #[tokio::test]
    async fn interrupt_on_an_idle_agent_is_a_no_op() {
        let (sup, id, dir) = shim_supervisor("interrupt-idle", ECHO_HARNESS);
        sup.start(id).await.unwrap();
        until("the agent to come up", || {
            !matches!(status_of(&sup, id), AgentStatus::Starting)
        })
        .await;
        let before = status_of(&sup, id);

        let status = sup.interrupt(id).await.unwrap();

        assert_eq!(
            status, before,
            "nothing was in flight, so interrupt must not change the status"
        );
        assert_eq!(
            runs(&dir),
            1,
            "an idle agent's process must not be killed by interrupt"
        );
    }

    /// Interrupting an agent with no process at all -- never started, or already stopped -- is
    /// equally a no-op: there is nothing to cancel.
    #[tokio::test]
    async fn interrupt_on_a_stopped_agent_reports_its_status_without_panicking() {
        let (sup, id, _dir) = shim_supervisor("interrupt-stopped", ECHO_HARNESS);
        let status = sup.interrupt(id).await.unwrap();
        assert_eq!(status, AgentStatus::default());
    }

    /// Waits for a file to exist and returns its contents, for a value a background process
    /// writes at an unpredictable moment (its own pid, here).
    async fn until_present(path: &std::path::Path) -> String {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        loop {
            if let Ok(s) = std::fs::read_to_string(path) {
                if !s.trim().is_empty() {
                    return s;
                }
            }
            if std::time::Instant::now() >= deadline {
                panic!("timed out waiting for {}", path.display());
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }
}
