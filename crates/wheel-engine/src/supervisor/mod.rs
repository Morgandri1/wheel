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

mod prompt;
pub use prompt::compose_prompt;

/// What the supervisor knows about one running agent.
struct Running {
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

pub struct Supervisor {
    cfg: Arc<Config>,
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
        Self {
            cfg,
            db,
            agents: Arc::new(AsyncMutex::new(HashMap::new())),
            harness,
            events,
        }
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
        // Stored credentials, if any. Absent is not an error: the harness may
        // hold OAuth credentials in its own config dir, and the authoritative
        // answer is its probe rather than our guess.
        for (k, v) in crate::auth::credential_env(&spec.config_dir, agent_cfg.harness) {
            cmd.env(k, v);
        }

        let mut child = cmd.spawn().context("spawning the harness")?;
        let stdin = child.stdin.take().expect("stdin was piped");
        let stdout = child.stdout.take().expect("stdout was piped");
        let stderr = child.stderr.take().expect("stderr was piped");

        *guard = Some(Running {
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
        let stderr_done = self.pump_stderr(agent, stderr);
        self.clone()
            .pump_stdout(agent, stdout, slot.clone(), stderr_done);

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
    ) {
        let db = self.db.clone();
        let harness = self.harness.clone();
        let bus = self.events.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let event = harness.parse_line(&line);
                match event {
                    HarnessEvent::Init { session_id } => {
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
                        {
                            let conn = db.lock().unwrap();
                            if let Some(mid) = finished {
                                if is_error {
                                    // Consumed, not requeued: poison must not loop.
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
                            set_status_db(
                                &conn,
                                agent,
                                if is_error {
                                    AgentStatus::Error
                                } else {
                                    AgentStatus::Idle
                                },
                                is_error.then(|| text.clone().unwrap_or_default()),
                            );
                        }

                        // The turn is over, so the next queued message may now
                        // be written. This is the only place delivery resumes:
                        // one message per turn, never mid-turn.
                        let _ = self.pump_queue(agent).await;
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
            self.reap(agent, slot, stderr_done).await;
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
    ) {
        // Wait for stderr so the classification below sees the whole message.
        // Without this the two tasks race and the reason is a coin flip.
        let captured = stderr_done.await.unwrap_or_default();

        let mut guard = slot.lock().await;
        let Some(mut running) = guard.take() else {
            // `stop()` already took the slot and set the status. Nothing to
            // settle, and overwriting its Stopped would be a lie.
            return;
        };
        let _ = running.child.wait().await;
        let in_flight = running.in_flight;
        drop(guard);

        let (status, detail) = match self.harness.classify_startup_failure(None, &captured) {
            // Environmental, not poison: the queued message stays queued and
            // is delivered on the next start once the operator authenticates.
            // Marking it error would consume the message and lose it.
            StartupFailure::NeedsAuth => (
                AgentStatus::NeedsAuth,
                Some("the harness has no usable credentials".to_string()),
            ),
            StartupFailure::Misconfigured(why) if !captured.trim().is_empty() => {
                (AgentStatus::Error, Some(why))
            }
            // Exited with nothing to say. A clean shutdown looks exactly like
            // this, so it is not reported as an error.
            StartupFailure::Misconfigured(_) => (AgentStatus::Stopped, None),
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
            set_status_db(&conn, agent, status, detail);
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
    ) -> tokio::task::JoinHandle<String> {
        let db = self.db.clone();
        let bus = self.events.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            let mut captured = String::new();
            while let Ok(Some(line)) = lines.next_line().await {
                captured.push_str(&line);
                captured.push('\n');
                let conn = db.lock().unwrap();
                // stderr is log material, never parsed as JSON.
                log_line_bus(&bus, &conn, agent, "stderr", &line);
            }
            captured
        })
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
            HarnessEvent::Unknown { raw: line.into() }
        }
        fn classify_startup_failure(&self, _code: Option<i32>, stderr: &str) -> StartupFailure {
            ClaudeDriver.classify_startup_failure(None, stderr)
        }
    }

    /// Builds a supervisor whose child is `script`, over an in-memory board
    /// holding one agent node. Returns the agent's id and its scratch dir.
    fn shim_supervisor(name: &str, script: &str) -> (Arc<Supervisor>, Uuid, std::path::PathBuf) {
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
        let node = wheel_core::Node::new(
            Uuid::new_v4(),
            name.parse().unwrap(),
            wheel_core::Position::default(),
            wheel_core::NodeConfig::Agent(wheel_core::AgentConfig {
                harness: wheel_core::Harness::Claude,
                system_prompt: "test".into(),
                ..Default::default()
            }),
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
}
