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

/// How much of a child's stdout is kept for classifying why it died. Enough
/// for a CLI's error banner, small enough that a runaway child cannot grow it.
const STARTUP_OUTPUT_TAIL: usize = 8 * 1024;

mod prompt;
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
    /// The message currently occupying the child, if any. Exactly one at a
    /// time: the next is written only after this turn's `result`.
    in_flight: Option<Uuid>,
    /// Consecutive user-lane deliveries, for the §3 fairness cap.
    consecutive_user: u32,
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
];

/// Where to look for the harness when the engine itself was started without a
/// `PATH`. Matches what the host uses for the engine.
const DEFAULT_PATH: &str = "/usr/local/bin:/usr/bin:/bin";

pub(crate) fn inherit_platform_env(cmd: &mut tokio::process::Command) {
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
        }
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
        let mut guard = slot.lock().await;

        if let Some(r) = guard.as_ref() {
            // Already running. Do NOT spawn a second process.
            let _ = r.session_id;
            return Ok(AgentStatus::Running);
        }

        let (node, resume) = {
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
            (node, state.session_id)
        };

        let agent_cfg = node
            .config
            .as_agent()
            .ok_or_else(|| anyhow::anyhow!("not an agent config"))?
            .clone();

        let run_dir = self.cfg.node_run_dir(agent);
        std::fs::create_dir_all(&run_dir)?;
        let config_dir = self.cfg.creds_dir().join(agent.to_string());
        std::fs::create_dir_all(&config_dir)?;

        // The composed prompt goes to a file, never argv.
        let prompt = {
            let conn = self.db.lock().unwrap();
            compose_prompt(&conn, &node, &self.cfg.project_id.to_string())?
        };
        let prompt_file = run_dir.join("prompt.txt");
        std::fs::write(&prompt_file, prompt).context("writing the composed prompt")?;

        let spec = SpawnSpec {
            node_id: agent,
            node_name: node.name.to_string(),
            model: agent_cfg.model.clone(),
            prompt_file,
            mcp_config: None,
            resume,
            config_dir,
            cwd: self.cfg.data_dir.clone(),
        };

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

        self.set_status(agent, AgentStatus::Starting, None);

        let mut cmd = tokio::process::Command::new(self.harness.program());
        cmd.args(self.harness.argv(&spec))
            .current_dir(&spec.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        // The child starts from an EMPTY environment and is given back only
        // what it needs (ADVERSARY F015). A process can always read its own
        // /proc/self/environ, so anything inherited here is readable by
        // untrusted code no matter which uid it runs as: the engine's own
        // WHEEL_ENGINE_SECRET and WHEEL_VAULT_KEY were both inherited, which
        // handed an agent the control-plane bearer and the key to every vault
        // in the project -- including vaults it had no wire to.
        //
        // An allowlist rather than a deny-list, so a variable added to the
        // engine's environment later is dropped by default rather than leaked
        // until somebody remembers to name it.
        cmd.env_clear();
        inherit_platform_env(&mut cmd);
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

        // A private crate cache per project. The toolchain in the image is
        // shared and immutable; what a tenant FETCHES is not, and a shared
        // CARGO_HOME would put one project's downloaded sources -- and its
        // registry credentials, if it ever configures any -- where the next
        // project can read them.
        let cargo_home = self.cfg.data_dir.join("cargo");
        std::fs::create_dir_all(&cargo_home).ok();
        cmd.env("CARGO_HOME", &cargo_home);
        // Stored credentials, if any. Absent is not an error: the harness may
        // hold OAuth credentials in its own config dir, and the authoritative
        // answer is its probe rather than our guess.
        for (k, v) in crate::auth::credential_env(&spec.config_dir, agent_cfg.harness) {
            cmd.env(k, v);
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
                crate::vault::env_for_agent(&conn, vk, agent)?
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
        *guard = Some(Running {
            run_id,
            session_id: None,
            stdin,
            child,
            in_flight: None,
            consecutive_user: 0,
        });
        drop(guard);

        // stderr is logged by its own task; the stdout task waits for it and
        // is the single owner of "this child has died", so the slot is cleared
        // and the status settled exactly once, in a defined order.
        let stderr_done = self.pump_stderr(agent, stderr, secrets.clone());
        self.clone()
            .pump_stdout(agent, stdout, slot.clone(), stderr_done, run_id, secrets);

        Ok(AgentStatus::Starting)
    }

    /// Stop an agent's child. Keeps the session id so a later start resumes.
    pub async fn stop(&self, agent: Uuid) -> Result<AgentStatus> {
        let slot = self.slot(agent).await;
        let mut guard = slot.lock().await;
        if let Some(mut r) = guard.take() {
            let _ = r.child.kill().await;
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

    /// Deliver the next queued message if the agent is idle.
    ///
    /// The ONLY path that writes to a child's stdin. Strictly one message per
    /// turn: while `in_flight` is set nothing further is written, so the
    /// operator's chat and inbound agent traffic can never interleave.
    pub async fn pump_queue(&self, agent: Uuid) -> Result<()> {
        let slot = self.slot(agent).await;
        let mut guard = slot.lock().await;
        let Some(running) = guard.as_mut() else {
            return Ok(()); // stopped or parked: the queue drains on next start
        };
        if running.in_flight.is_some() {
            return Ok(()); // mid-turn
        }

        let next = {
            let conn = self.db.lock().unwrap();
            messages::next_for_delivery(&conn, agent, running.consecutive_user)?
        };
        let Some(msg) = next else { return Ok(()) };

        let line = self.harness.encode_turn(&msg.envelope());
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
            let mut tail = String::new();
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
                if tail.len() + line.len() + 1 > STARTUP_OUTPUT_TAIL {
                    let drop_to = tail.len().min(line.len() + 1);
                    tail.drain(..drop_to);
                }
                tail.push_str(&line);
                tail.push('\n');
                let event = harness.parse_line(&line);
                match event {
                    HarnessEvent::Init { session_id } => {
                        initialised = true;
                        let mut g = slot.lock().await;
                        if let Some(r) = g.as_mut() {
                            r.session_id = Some(session_id.clone());
                        }
                        let conn = db.lock().unwrap();
                        set_session(&conn, agent, &session_id);
                        set_status_db(&conn, agent, AgentStatus::Idle, None);
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
                        drop(g);

                        // Scoped so the sqlite guard cannot be held across the
                        // await below: a rusqlite Connection is not Send, and
                        // holding its guard would make this task unspawnable.
                        // A harness error is not automatically the MESSAGE's
                        // fault. "Not logged in" arrives as a perfectly normal
                        // `result` with is_error, and consuming the message on
                        // that basis loses the operator's work to a setup
                        // problem they are about to fix. Environmental
                        // failures requeue; genuine task errors are consumed,
                        // because poison must not loop.
                        let environmental = is_error
                            && matches!(
                                harness.classify_startup_failure(
                                    None,
                                    text.as_deref().unwrap_or_default()
                                ),
                                StartupFailure::NeedsAuth
                            );

                        {
                            let conn = db.lock().unwrap();
                            if let Some(mid) = finished {
                                if environmental {
                                    messages::requeue_undelivered(
                                        &conn,
                                        mid,
                                        text.as_deref()
                                            .unwrap_or("the harness could not run this turn"),
                                    )
                                    .ok();
                                    publish_message(&bus, &conn, mid);
                                } else if is_error {
                                    messages::mark_error(
                                        &conn,
                                        mid,
                                        text.as_deref().unwrap_or("harness reported an error"),
                                    )
                                    .ok();
                                } else {
                                    messages::advance(&conn, mid, MessageState::Consumed).ok();
                                    publish_message(&bus, &conn, mid);
                                }
                            }
                            let (status, detail) = if environmental {
                                (
                                    AgentStatus::NeedsAuth,
                                    Some("the harness has no usable credentials".to_string()),
                                )
                            } else if is_error {
                                (AgentStatus::Error, Some(text.clone().unwrap_or_default()))
                            } else {
                                (AgentStatus::Idle, None)
                            };
                            set_status_db(&conn, agent, status, detail);
                            publish_state(&bus, &conn, agent);
                        }

                        if environmental {
                            // Nothing more can run until credentials exist, and
                            // draining the queue into the same failure would
                            // requeue every message in turn for no reason.
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
                        } else {
                            let _ = self.pump_queue(agent).await;
                        }
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
            self.reap(agent, slot, stderr_done, run_id, tail, initialised)
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
            // Anything written to the dying child never ran a turn, so it goes
            // back on the queue rather than being lost as though it had been
            // handled.
            let n = messages::requeue_all_undelivered(
                &conn,
                agent,
                "the harness exited before this message could be processed",
            )
            .unwrap_or(0);
            if n > 0 {
                tracing::info!(agent = %agent, requeued = n, in_flight = ?in_flight, "returned in-flight messages to the queue");
            }
            // A token outliving its process is a credential with no owner.
            let _ = crate::db::tokens::revoke(&conn, agent);
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
                let _ = r.child.kill().await;
            }
        }
        {
            let conn = self.db.lock().unwrap();
            clear_session(&conn, agent);
        }
        let status = self.start(agent).await?;
        let _ = self.pump_queue(agent).await;
        Ok(status)
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
    let _ = conn.execute(
        "INSERT INTO agent_state (node_id,status,last_activity,last_error)
         VALUES (?1,?2,?3,?4)
         ON CONFLICT(node_id) DO UPDATE SET status=?2, last_activity=?3, last_error=?4",
        rusqlite::params![
            agent.to_string(),
            status.as_str(),
            wheel_core::Timestamp::now().to_rfc3339(),
            err,
        ],
    );
}

/// Forget the resumable session, so the next start is a NEW context rather
/// than a `--resume` of the one being discarded.
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
        });
        let sup = Arc::new(Supervisor::with_harness(
            cfg,
            Arc::new(Mutex::new(conn)),
            Arc::new(crate::events::Bus::new()),
            Arc::new(ShimDriver {
                program: program.display().to_string(),
            }),
        ));
        (sup, id, dir)
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
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            if cond() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!("timed out waiting for {what}");
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
        std::env::set_var("WHEEL_ENGINE_SECRET", "engine-bearer-must-not-leak");
        std::env::set_var("WHEEL_VAULT_KEY", "dmF1bHQta2V5LW11c3Qtbm90LWxlYWs=");

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
        assert!(env.contains("WHEEL_NODE="), "the child lost its identity");
        assert!(
            env.contains("WHEEL_TOKEN_FILE="),
            "the child lost its capability token"
        );

        sup.stop(id).await.ok();
        std::fs::remove_dir_all(&dir).ok();
    }

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
        )
        .unwrap();
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

        // One turn, then the restart: a second child for the same agent.
        until("the ephemeral restart to spawn a fresh child", || {
            count(&dir.join("runs")) == 2
        })
        .await;

        // ...and it did NOT resume: both children started clean.
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
}
