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
        Self {
            cfg,
            db,
            agents: Arc::new(AsyncMutex::new(HashMap::new())),
            harness: Arc::new(ClaudeDriver),
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

        let mut cmd = tokio::process::Command::new("claude");
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

        self.clone().pump_stdout(agent, stdout, slot.clone());
        self.pump_stderr(agent, stderr);

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
        });
    }

    fn pump_stderr(&self, agent: Uuid, stderr: tokio::process::ChildStderr) {
        let db = self.db.clone();
        let harness = self.harness.clone();
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
            // The child's stderr closed: classify why it went away.
            //
            // Both branches matter. Matching only Misconfigured here silently
            // DROPPED needs_auth, so an agent with no credentials sat in
            // whatever status it happened to hold and the operator was never
            // told to authenticate.
            if !captured.is_empty() {
                let (status, detail) = match harness.classify_startup_failure(None, &captured) {
                    // Environmental, not poison: the queued message stays
                    // queued and is delivered on the next start once the
                    // operator authenticates. Marking it error would consume
                    // the message and lose it.
                    StartupFailure::NeedsAuth => (
                        AgentStatus::NeedsAuth,
                        Some("the harness has no usable credentials".to_string()),
                    ),
                    StartupFailure::Misconfigured(why) => (AgentStatus::Error, Some(why)),
                };
                let conn = db.lock().unwrap();
                // Anything written to the dying child never ran a turn, so it
                // goes back on the queue rather than being lost as though it
                // had been handled.
                let n = messages::requeue_all_undelivered(
                    &conn,
                    agent,
                    "the harness exited before this message could be processed",
                )
                .unwrap_or(0);
                if n > 0 {
                    tracing::info!(agent = %agent, requeued = n, "returned in-flight messages to the queue");
                }
                set_status_db(&conn, agent, status, detail);
                publish_state(&bus, &conn, agent);
            }
        });
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
