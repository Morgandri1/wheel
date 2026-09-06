//! Paste-code OAuth: signing an agent node in to a real Anthropic account.
//!
//! The CLI's login is interactive, but not in a way that needs a terminal. Run
//! headless it prints an authorize URL, then blocks reading a code on stdin:
//!
//! ```text
//! Opening browser to sign in…
//! If the browser didn't open, visit: https://claude.com/cai/oauth/authorize?...&state=...
//! Paste code here if prompted >
//! ```
//!
//! The redirect target is Anthropic-hosted, so the container never needs a
//! reachable localhost — the browser shows the user a code and they paste it
//! back. That makes the flow two calls with a LIVE CHILD between them, which
//! is the only real complexity here: `auth/begin` must keep a process alive
//! until `auth/complete` feeds it, and must not leak one if that never comes.

use std::{collections::HashMap, path::Path, process::Stdio, time::Duration};

use anyhow::Result;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin},
    sync::Mutex,
    time::Instant,
};
use uuid::Uuid;

/// How long a login may sit unfinished. The user has to visit a URL, sign in
/// and copy a code; generous, but not unbounded — the child is a real process.
pub const SESSION_TTL: Duration = Duration::from_secs(15 * 60);

/// How long to wait for the CLI to print its authorize URL.
const URL_TIMEOUT: Duration = Duration::from_secs(30);

/// What tests use instead. Deliberately far beyond anything a stub needs: the
/// property under test is "the URL was found", and `begin` returning `Ok` is
/// already proof it was found before the deadline. Inheriting the production
/// budget only makes the test fail when the machine is busy, which teaches
/// everyone to ignore it.
#[cfg(test)]
const GENEROUS_TEST_URL_TIMEOUT: Duration = Duration::from_secs(120);

/// How long to wait for a verdict on a submitted code.
///
/// The CLI does not necessarily EXIT when it rejects one -- it prints the
/// reason and prompts again -- so waiting for exit waits forever. This bounds
/// the wait to well inside what the API and the browser will tolerate: a
/// request that never answers is reported to the operator as a gateway
/// timeout, which tells them nothing about their code being wrong.
const CODE_TIMEOUT: Duration = Duration::from_secs(20);

/// How often to look for a verdict while waiting.
const VERDICT_POLL: Duration = Duration::from_millis(100);

/// What the CLI says when it did not accept a code. Matched only against
/// output produced AFTER the code was submitted, so the greeting cannot look
/// like a rejection.
const REJECTION_MARKERS: &[&str] = &[
    "login failed",
    "invalid",
    "error",
    "failed",
    "expired",
    "denied",
    "unauthorized",
];

/// How long to wait for the child's own output after it has exited.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

struct Pending {
    session: Uuid,
    url: String,
    started: Instant,
    child: Child,
    stdin: ChildStdin,
    /// Everything the child has said, for diagnosing a rejected code.
    output: std::sync::Arc<std::sync::Mutex<String>>,
    /// The tasks draining the child's pipes. They finish when the pipes close,
    /// which is what makes `output` complete rather than merely current.
    pumps: Vec<tokio::task::JoinHandle<()>>,
}

#[derive(Debug, thiserror::Error)]
pub enum LoginError {
    #[error("no login is in progress for this agent; call auth/begin first")]
    NoSession,
    #[error("this login expired; call auth/begin again")]
    Expired,
    #[error("that code was not accepted: {0}")]
    Rejected(String),
    #[error("the login did not finish in time")]
    Timeout,
    #[error(
        "the login process did not respond to that code within {}s; \
         start the sign-in again",
        CODE_TIMEOUT.as_secs()
    )]
    NoResponse,
    #[error("{0}")]
    Spawn(String),
}

type Sessions = std::sync::Arc<Mutex<HashMap<Uuid, Pending>>>;

pub struct LoginSessions {
    inner: Sessions,
    /// A field for the same reason `ttl` is: these tests spawn real shells on
    /// a host running six agents, where a process can take twenty seconds to
    /// start. A production constant is not a budget a test should inherit.
    url_timeout: Duration,
    /// A field rather than a constant so the expiry path can be tested at
    /// speed. Production always uses `SESSION_TTL`.
    ttl: Duration,
}

impl Default for LoginSessions {
    fn default() -> Self {
        Self {
            inner: Sessions::default(),
            ttl: SESSION_TTL,
            // Under test, a budget the machine cannot blow through. These
            // tests spawn real shells on a host running six agents, and a
            // production constant is not a budget a test should inherit.
            #[cfg(test)]
            url_timeout: GENEROUS_TEST_URL_TIMEOUT,
            #[cfg(not(test))]
            url_timeout: URL_TIMEOUT,
        }
    }
}

impl LoginSessions {
    /// Start a login and return the URL the user must visit.
    ///
    /// Any login already in flight for this node is killed first: a second
    /// `begin` means the user gave up on the first, and leaving that child
    /// alive would leak a process per retry.
    pub async fn begin(
        &self,
        node: Uuid,
        program: &str,
        config_dir: &Path,
    ) -> Result<(Uuid, String), LoginError> {
        self.cancel(node).await;

        std::fs::create_dir_all(config_dir).ok();
        let mut cmd = crate::supervisor::child_command(program);
        cmd.args(["auth", "login", "--claudeai"])
            // The node's own config dir, so this login belongs to this agent
            // and not to every agent in the sandbox.
            .env("CLAUDE_CONFIG_DIR", config_dir)
            .env("HOME", config_dir)
            .env("IS_SANDBOX", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        let mut child = cmd
            .spawn()
            .map_err(|e| LoginError::Spawn(format!("could not start {program}: {e}")))?;
        let stdin = child.stdin.take().expect("stdin piped");
        let stdout = child.stdout.take().expect("stdout piped");
        let stderr = child.stderr.take().expect("stderr piped");

        // Pump BOTH streams from the start, and look for the URL in whatever
        // arrives. The URL was read from stdout only, which assumed something
        // about a CLI this engine does not own: an interactive prompt
        // routinely goes to stderr, and if it does, `begin` waits the full
        // timeout and answers 504 -- an operator staring at a gateway error
        // for a sign-in that was working fine on the other side of the pipe.
        let output = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let mut pumps = spawn_pumps(
            vec![
                Box::new(stdout) as Box<dyn tokio::io::AsyncRead + Unpin + Send>,
                Box::new(stderr) as Box<dyn tokio::io::AsyncRead + Unpin + Send>,
            ],
            &output,
        );

        let url = match wait_for_url(&output, &mut child, &mut pumps, self.url_timeout).await {
            Ok(url) => url,
            Err(e) => {
                let _ = child.kill().await;
                return Err(e);
            }
        };

        let session = Uuid::new_v4();
        self.inner.lock().await.insert(
            node,
            Pending {
                session,
                url: url.clone(),
                started: Instant::now(),
                child,
                stdin,
                output,
                pumps,
            },
        );
        self.arm_expiry(node, session);
        Ok((session, url))
    }

    /// Feed the pasted code to the waiting child and report what it made of it.
    pub async fn complete(
        &self,
        node: Uuid,
        session: Option<Uuid>,
        code: &str,
    ) -> Result<(), LoginError> {
        let mut pending = {
            let mut guard = self.inner.lock().await;
            guard.remove(&node).ok_or(LoginError::NoSession)?
        };

        if pending.started.elapsed() > self.ttl {
            let _ = pending.child.kill().await;
            return Err(LoginError::Expired);
        }
        // A stale tab finishing an old login would otherwise complete a session
        // the user has already restarted.
        if let Some(s) = session {
            if s != pending.session {
                let _ = pending.child.kill().await;
                return Err(LoginError::Expired);
            }
        }

        // Everything after this point is the CLI's response to THIS code, so
        // a rejection can be recognised without the greeting matching first.
        let before = pending.output.lock().map(|o| o.len()).unwrap_or(0);

        let line = format!("{}\n", code.trim());
        if pending.stdin.write_all(line.as_bytes()).await.is_err() {
            // The child is already gone, which means it has already said why.
            // Reporting "the login process had already exited" would replace
            // the CLI's own reason with a description of our plumbing.
            let _ = pending.child.kill().await;
            drain(pending.pumps).await;
            return Err(LoginError::Rejected(tail(&pending.output)));
        }
        let _ = pending.stdin.flush().await;

        verdict(&mut pending, before).await
    }

    /// Collect this login once its TTL is up, so `SESSION_TTL` is a fact
    /// rather than a comment.
    ///
    /// A timer per login rather than one sweep loop over all of them: the
    /// engine is required to idle at ~0 CPU, and a sandbox where nobody is
    /// signing in should not be waking up to discover that. `begin` also
    /// evicts, but a user who starts one login and walks away never calls
    /// `begin` again — so without this, nothing would ever reap that child.
    fn arm_expiry(&self, node: Uuid, session: Uuid) {
        let sessions = self.inner.clone();
        let ttl = self.ttl;
        tokio::spawn(async move {
            tokio::time::sleep(ttl).await;
            let mut guard = sessions.lock().await;
            // Only if it is still THIS login: a retry replaces the entry, and
            // the new child must outlive the old one's timer.
            if guard.get(&node).is_some_and(|p| p.session == session) {
                // Dropping the Pending kills the child: `kill_on_drop`.
                guard.remove(&node);
            }
        });
    }

    /// Kill any login in flight for a node. Safe to call when there is none.
    pub async fn cancel(&self, node: Uuid) {
        if let Some(mut p) = self.inner.lock().await.remove(&node) {
            let _ = p.child.kill().await;
        }
    }

    /// Kill every login in flight. Called when the engine is going away, so a
    /// `claude auth login` child does not outlive the process that started it
    /// and sit waiting on a stdin nobody will ever write to.
    pub async fn shutdown(&self) {
        let mut guard = self.inner.lock().await;
        let ids: Vec<Uuid> = guard.keys().copied().collect();
        for id in ids {
            if let Some(mut p) = guard.remove(&id) {
                let _ = p.child.kill().await;
                let _ = p.child.wait().await;
            }
        }
    }

    /// Drop logins that have outlived their TTL, killing their children.
    ///
    /// Called on `begin`, so an abandoned login cannot hold a process forever
    /// just because nobody came back to it.
    pub async fn evict_expired(&self) {
        let mut guard = self.inner.lock().await;
        let stale: Vec<Uuid> = guard
            .iter()
            .filter(|(_, p)| p.started.elapsed() > self.ttl)
            .map(|(id, _)| *id)
            .collect();
        for id in stale {
            if let Some(mut p) = guard.remove(&id) {
                let _ = p.child.kill().await;
            }
        }
    }

    #[cfg(test)]
    fn with_ttl(ttl: Duration) -> Self {
        Self {
            inner: Sessions::default(),
            ttl,
            url_timeout: GENEROUS_TEST_URL_TIMEOUT,
        }
    }

    #[cfg(test)]
    async fn len(&self) -> usize {
        self.inner.lock().await.len()
    }
}

/// Decide what happened to a submitted code, without waiting for an exit that
/// may never come.
///
/// Three outcomes race: the child exits (its status is the answer), the child
/// prints a rejection and prompts again (the common failure, and the one that
/// used to hang until the caller gave up), or nothing happens at all. A login
/// that answers nothing is reported as such rather than as a gateway timeout
/// somewhere further up, where nobody can tell it was the code that was wrong.
async fn verdict(pending: &mut Pending, before: usize) -> Result<(), LoginError> {
    let deadline = Instant::now() + CODE_TIMEOUT;
    loop {
        match pending.child.try_wait() {
            Ok(Some(status)) => {
                let pumps = std::mem::take(&mut pending.pumps);
                drain(pumps).await;
                return if status.success() {
                    Ok(())
                } else {
                    Err(LoginError::Rejected(tail_since(&pending.output, before)))
                };
            }
            Err(e) => return Err(LoginError::Rejected(e.to_string())),
            Ok(None) => {}
        }

        if let Some(reason) = rejection_since(&pending.output, before) {
            // It is still running and waiting for another code. We are not
            // going to give it one, so it must not outlive this answer.
            let _ = pending.child.kill().await;
            return Err(LoginError::Rejected(reason));
        }

        if Instant::now() >= deadline {
            let _ = pending.child.kill().await;
            return Err(LoginError::NoResponse);
        }
        tokio::time::sleep(VERDICT_POLL).await;
    }
}

/// Everything the child said since `from`, if it looks like a refusal.
fn rejection_since(
    output: &std::sync::Arc<std::sync::Mutex<String>>,
    from: usize,
) -> Option<String> {
    let text = output.lock().ok()?.clone();
    let fresh = text.get(from..)?.trim();
    if fresh.is_empty() {
        return None;
    }
    let lower = fresh.to_ascii_lowercase();
    REJECTION_MARKERS
        .iter()
        .any(|m| lower.contains(m))
        .then(|| clean(fresh))
}

fn tail_since(output: &std::sync::Arc<std::sync::Mutex<String>>, from: usize) -> String {
    let text = output.lock().map(|o| o.clone()).unwrap_or_default();
    let fresh = text.get(from..).unwrap_or("").trim();
    if fresh.is_empty() {
        // Nothing new: the whole transcript is better than nothing at all.
        return tail(output);
    }
    clean(fresh)
}

/// Start a reader per stream, appending everything into one buffer.
///
/// Extracted so a test can create the exact situation the drain exists for:
/// readers that have not yet been polled while the child is already gone.
fn spawn_pumps(
    streams: Vec<Box<dyn tokio::io::AsyncRead + Unpin + Send>>,
    output: &std::sync::Arc<std::sync::Mutex<String>>,
) -> Vec<tokio::task::JoinHandle<()>> {
    streams
        .into_iter()
        .map(|stream| {
            let output = output.clone();
            tokio::spawn(async move {
                // read_until rather than lines(): the prompt the CLI writes
                // before waiting for input carries no newline, so a
                // line-oriented reader never yields it -- and it is the thing
                // that says the CLI is ready for a code.
                let mut reader = BufReader::new(stream);
                let mut buf = Vec::new();
                loop {
                    buf.clear();
                    match reader.read_until(b'\n', &mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {
                            if let Ok(mut o) = output.lock() {
                                o.push_str(&String::from_utf8_lossy(&buf));
                            }
                        }
                    }
                }
            })
        })
        .collect()
}

/// Wait for the pipe readers to finish before reporting what the child said.
///
/// `wait()` returns when the process exits, which is NOT when its output has
/// been read: the pumps are separate tasks, and the last line can still be in
/// flight. Without this the rejection reason is whatever happened to have
/// arrived -- often nothing, so the operator is told "the CLI rejected it
/// without saying why" while the CLI's actual reason is a few microseconds
/// behind. It won the race on macOS and lost it on Linux CI, which is the
/// clearest possible sign it was a race and not a platform difference.
///
/// The pipes are already closed by the exit, so these finish immediately; the
/// timeout is only so a wedged reader cannot hold the login open.
async fn drain(pumps: Vec<tokio::task::JoinHandle<()>>) {
    let _ = tokio::time::timeout(DRAIN_TIMEOUT, async {
        for p in pumps {
            let _ = p.await;
        }
    })
    .await;
}

/// The CLI's stdin prompt, which carries no newline and therefore ends up
/// glued to the front of whatever it says next.
const PASTE_PROMPT: &str = "Paste code here if prompted >";

fn tail(output: &std::sync::Arc<std::sync::Mutex<String>>) -> String {
    let text = output.lock().map(|o| o.clone()).unwrap_or_default();
    clean(&text)
}

/// The last few lines, with the CLI's stdin prompt stripped out.
///
/// Without this the operator is told "that code was not accepted: Paste code
/// here if prompted > Login failed: ..." — the prompt is furniture, and the
/// reason is the part they need.
fn clean(text: &str) -> String {
    let text = text.replace(PASTE_PROMPT, " ");
    let trimmed = text.trim();
    if trimmed.is_empty() {
        "the CLI rejected it without saying why".to_string()
    } else {
        trimmed.rsplit('\n').take(3).collect::<Vec<_>>().join(" ")
    }
}

/// Pull the authorize URL out of the CLI's greeting.
///
/// Matches on `https://` rather than the sentence around it: the wording
/// ("If the browser didn't open, visit: ") is cosmetic and has no contract
/// behind it, while a URL on its own line does.
/// Pull the authorize URL out of whatever the CLI has said so far.
///
/// Matches on `https://` rather than the sentence around it: the wording
/// ("If the browser didn't open, visit: ") is cosmetic and has no contract
/// behind it, while a URL does.
fn extract_url(text: &str) -> Option<String> {
    let start = text.find("https://")?;
    let rest = &text[start..];
    // Stop at the first whitespace: the URL may be followed by more output
    // on the same read, and a trailing prompt must not become part of it.
    let end = rest.find(|c: char| c.is_whitespace()).unwrap_or(rest.len());
    let url = rest[..end].trim_end_matches(['.', ',']);
    (!url.is_empty()).then(|| url.to_string())
}

/// Watch everything the child says until an authorize URL appears.
///
/// Polls the shared buffer rather than owning a pipe, so it does not care
/// which stream the CLI chose — and notices if the child gives up first,
/// which is a different failure from "it is taking a while" and deserves the
/// child's own words rather than a timeout.
async fn wait_for_url(
    output: &std::sync::Arc<std::sync::Mutex<String>>,
    child: &mut Child,
    pumps: &mut Vec<tokio::task::JoinHandle<()>>,
    budget: Duration,
) -> Result<String, LoginError> {
    let deadline = Instant::now() + budget;
    loop {
        let text = output.lock().map(|o| o.clone()).unwrap_or_default();
        if let Some(url) = extract_url(&text) {
            return Ok(url);
        }
        if let Ok(Some(_)) = child.try_wait() {
            // Exit is not the same event as "its output has been read": the
            // pumps are separate tasks and the last line can still be in
            // flight. Without this the operator is told the login "ended
            // before printing a URL" with NO reason attached, while the
            // reason is microseconds behind — the same race that was fixed on
            // the `complete` path and left here. QA's gate caught it failing
            // under load.
            //
            // Demonstrated, not merely reasoned: removing this line makes
            // `a_dead_childs_reason_is_read_before_it_is_reported` fail with
            // the exact text QA's gate reported — "the CLI rejected it without
            // saying why".
            drain(std::mem::take(pumps)).await;
            let said = clean(&output.lock().map(|o| o.clone()).unwrap_or_default());
            return Err(LoginError::Spawn(format!(
                "the login process ended before printing a URL: {said}"
            )));
        }
        if Instant::now() >= deadline {
            return Err(LoginError::Timeout);
        }
        tokio::time::sleep(VERDICT_POLL).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact bytes `claude 2.1.261` prints, captured from a real run.
    const REAL_GREETING: &str = "Opening browser to sign in…\n\
        If the browser didn't open, visit: https://claude.com/cai/oauth/authorize?code=true&client_id=9d1c250a-e61b-44d9-88ed-5944d1962f5e&response_type=code&redirect_uri=https%3A%2F%2Fplatform.claude.com%2Foauth%2Fcode%2Fcallback&scope=org%3Acreate_api_key+user%3Aprofile&code_challenge=pyIagh&code_challenge_method=S256&state=m9gz12oZ\n\
        Paste code here if prompted > ";

    #[test]
    fn the_url_is_pulled_out_of_the_real_cli_greeting() {
        let url = extract_url(REAL_GREETING).unwrap();
        assert!(url.starts_with("https://claude.com/cai/oauth/authorize?"));
        // The whole query string matters: without `state` the callback cannot
        // be tied back to this attempt.
        assert!(url.contains("state=m9gz12oZ"), "state must survive: {url}");
        assert!(url.contains("code_challenge=pyIagh"), "PKCE must survive");
        // The prose around it must not be dragged in, and neither must the
        // prompt that follows it on the next line.
        assert!(!url.contains("visit"), "prose leaked into the url: {url}");
        assert!(
            !url.contains("Paste"),
            "the prompt leaked into the url: {url}"
        );
        assert!(!url.contains('\n'));
    }

    #[test]
    fn nothing_that_is_not_a_url_is_mistaken_for_one() {
        assert!(extract_url("").is_none());
        assert!(extract_url("Opening browser to sign in…").is_none());
        assert!(extract_url("some unrelated failure").is_none());
    }

    /// The bug this was: the URL was read from stdout ONLY. An interactive
    /// CLI routinely prompts on stderr, and if it does, `begin` waits the
    /// full 30s and answers 504 — which the operator sees as a broken
    /// service for a sign-in that was working fine on the other side of the
    /// pipe. Whichever stream it chooses, we must find it.
    #[tokio::test]
    async fn the_url_is_found_even_when_the_cli_prints_it_to_stderr() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!(
            "wheel-oauth-stderr-{}-{}",
            std::process::id(),
            Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let program = dir.join("claude-stub");
        std::fs::write(
            &program,
            "#!/bin/sh\n\
             echo 'Opening browser to sign in…' >&2\n\
             echo 'visit: https://claude.com/cai/oauth/authorize?state=onstderr' >&2\n\
             printf 'Paste code here if prompted > ' >&2\n\
             read code\n\
             exit 0\n",
        )
        .unwrap();
        std::fs::set_permissions(&program, PermissionsExt::from_mode(0o755)).unwrap();

        let s = LoginSessions::default();
        let (_, url) = s
            .begin(
                Uuid::new_v4(),
                &program.display().to_string(),
                &dir.join("creds"),
            )
            .await
            .unwrap();

        assert!(url.contains("state=onstderr"), "{url}");
        // No timing assertion: `begin` returning Ok IS the proof that the URL
        // was found before the deadline, and anything tighter only fails when
        // the machine is busy.
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A child that dies without offering a URL must report ITS reason, not
    /// a timeout — those send an operator to completely different places.
    #[tokio::test]
    async fn a_login_that_dies_without_a_url_reports_what_it_said() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!(
            "wheel-oauth-dies-{}-{}",
            std::process::id(),
            Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let program = dir.join("claude-stub");
        // One line and an immediate exit: the shape that failed in QA's gate.
        //
        // This test asserts the PROPERTY through the public entry point but
        // cannot force the race — the window is a scheduling accident here.
        // `a_dead_childs_reason_is_read_before_it_is_reported` constructs it
        // deterministically instead.
        std::fs::write(
            &program,
            "#!/bin/sh\necho 'some unrelated failure' >&2\nexit 1\n",
        )
        .unwrap();
        std::fs::set_permissions(&program, PermissionsExt::from_mode(0o755)).unwrap();

        let s = LoginSessions::default();
        let err = s
            .begin(
                Uuid::new_v4(),
                &program.display().to_string(),
                &dir.join("creds"),
            )
            .await
            .unwrap_err();

        let msg = err.to_string();
        assert!(msg.contains("ended before printing a URL"), "{msg}");
        assert!(
            msg.contains("some unrelated failure"),
            "the reason must be carried to the operator: {msg}"
        );
        // Reporting the child's own words rather than a timeout is the
        // property; the error variant proves which path ran.
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn the_reason_does_not_carry_the_cli_prompt_as_furniture() {
        let out = std::sync::Arc::new(std::sync::Mutex::new(
            "Paste code here if prompted > Login failed: Request failed with status code 400\n"
                .to_string(),
        ));
        let reason = tail(&out);
        assert!(
            reason.starts_with("Login failed"),
            "the reason must lead with the reason: {reason}"
        );
        assert!(!reason.contains("Paste code here"));
    }

    #[tokio::test]
    async fn completing_without_beginning_is_a_clear_error() {
        let s = LoginSessions::default();
        let err = s.complete(Uuid::new_v4(), None, "code").await.unwrap_err();
        assert!(matches!(err, LoginError::NoSession));
    }

    #[tokio::test]
    async fn a_second_begin_replaces_the_first_rather_than_leaking_it() {
        let s = LoginSessions::default();
        let node = Uuid::new_v4();
        let dir = std::env::temp_dir().join(format!("wheel-oauth-{}", std::process::id()));

        // `true` prints no URL, so begin fails — but the important part is
        // that a failed begin leaves nothing behind to leak.
        let _ = s.begin(node, "true", &dir).await;
        assert_eq!(s.len().await, 0, "a failed login must not be retained");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn a_missing_binary_is_reported_not_panicked() {
        let s = LoginSessions::default();
        let dir = std::env::temp_dir().join("wheel-oauth-missing");
        let err = s
            .begin(Uuid::new_v4(), "definitely-not-a-real-binary", &dir)
            .await
            .unwrap_err();
        assert!(matches!(err, LoginError::Spawn(_)), "{err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A stub that speaks the CLI's real greeting, then accepts or rejects the
    /// pasted code. Exercises the whole two-call flow with a live child in
    /// between, which is the only part of this that can really go wrong.
    fn stub_cli(name: &str, accept: bool) -> (String, std::path::PathBuf) {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!(
            "wheel-oauth-stub-{name}-{}-{}",
            std::process::id(),
            Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("claude-stub");
        let body = format!(
            "#!/bin/sh\n\
             echo 'Opening browser to sign in…'\n\
             echo 'If the browser didn'\\''t open, visit: https://claude.com/cai/oauth/authorize?code=true&state=abc123'\n\
             printf 'Paste code here if prompted > '\n\
             read code\n\
             if [ \"$code\" = 'good#abc123' ] && [ {accept} -eq 1 ]; then\n\
               mkdir -p \"$CLAUDE_CONFIG_DIR\"\n\
               echo '{{}}' > \"$CLAUDE_CONFIG_DIR/.credentials.json\"\n\
               exit 0\n\
             fi\n\
             echo 'OAuth token exchange failed: 400' >&2\n\
             exit 1\n",
            accept = if accept { 1 } else { 0 }
        );
        std::fs::write(&path, body).unwrap();
        std::fs::set_permissions(&path, PermissionsExt::from_mode(0o755)).unwrap();
        (path.display().to_string(), dir)
    }

    #[tokio::test]
    async fn a_good_code_completes_the_login_and_leaves_credentials() {
        let (program, dir) = stub_cli("good", true);
        let creds = dir.join("creds");
        let s = LoginSessions::default();
        let node = Uuid::new_v4();

        let (session, url) = s.begin(node, &program, &creds).await.unwrap();
        assert!(url.starts_with("https://claude.com/cai/oauth/authorize?"));
        assert_eq!(
            s.len().await,
            1,
            "the child must be kept alive between calls"
        );

        s.complete(node, Some(session), "good#abc123")
            .await
            .unwrap();
        assert!(
            creds.join(".credentials.json").exists(),
            "the CLI must have written credentials into the NODE's own dir"
        );
        assert_eq!(s.len().await, 0, "a finished login must not be retained");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn a_bad_code_is_rejected_with_the_reason_the_cli_gave() {
        let (program, dir) = stub_cli("bad", false);
        let creds = dir.join("creds");
        let s = LoginSessions::default();
        let node = Uuid::new_v4();

        let (session, _) = s.begin(node, &program, &creds).await.unwrap();
        let err = s
            .complete(node, Some(session), "wrong#abc123")
            .await
            .unwrap_err();
        match err {
            LoginError::Rejected(why) => assert!(
                why.contains("400"),
                "the operator needs the CLI's own reason, got: {why}"
            ),
            other => panic!("expected a rejection, got {other}"),
        }
        // A rejected attempt must not leave a child holding the node hostage.
        assert_eq!(s.len().await, 0);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The regression CI caught. A child that writes its reason and exits
    /// immediately loses a race with the task reading its pipe, so the
    /// operator was told "the CLI rejected it without saying why" while the
    /// real reason sat unread. This stub writes enough that the pump cannot
    /// possibly have finished by the time `wait()` returns.
    #[tokio::test]
    async fn the_reason_survives_a_child_that_exits_the_instant_it_speaks() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!(
            "wheel-oauth-race-{}-{}",
            std::process::id(),
            Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let program = dir.join("claude-stub");
        std::fs::write(
            &program,
            "#!/bin/sh\n\
             echo 'Opening browser to sign in…'\n\
             echo 'visit: https://claude.com/cai/oauth/authorize?state=abc123'\n\
             printf 'Paste code here if prompted > '\n\
             read code\n\
             i=0\n\
             while [ $i -lt 200 ]; do echo \"noise line $i\" >&2; i=$((i+1)); done\n\
             echo 'OAuth token exchange failed: 400 invalid_grant' >&2\n\
             exit 1\n",
        )
        .unwrap();
        std::fs::set_permissions(&program, PermissionsExt::from_mode(0o755)).unwrap();

        let s = LoginSessions::default();
        let node = Uuid::new_v4();
        let (session, _) = s
            .begin(node, &program.display().to_string(), &dir.join("creds"))
            .await
            .unwrap();

        match s.complete(node, Some(session), "wrong").await.unwrap_err() {
            LoginError::Rejected(why) => {
                assert!(
                    why.contains("400") && why.contains("invalid_grant"),
                    "the CLI's own last words must reach the operator, got: {why}"
                );
            }
            other => panic!("expected a rejection, got {other}"),
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// S2 from Web: a bad code never answered. The real CLI does not EXIT when
    /// it rejects one -- it prints the reason and prompts again -- so waiting
    /// for an exit waits until the caller gives up, and the operator is shown
    /// a gateway timeout instead of "that code was wrong".
    #[tokio::test]
    async fn a_rejected_code_answers_even_though_the_cli_keeps_running() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!(
            "wheel-oauth-reprompt-{}-{}",
            std::process::id(),
            Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let program = dir.join("claude-stub");
        // Rejects, then waits for another code -- forever, like the real one.
        std::fs::write(
            &program,
            "#!/bin/sh\n\
             echo 'Opening browser to sign in…'\n\
             echo 'visit: https://claude.com/cai/oauth/authorize?state=abc123'\n\
             while true; do\n\
               printf 'Paste code here if prompted > '\n\
               read code || exit 0\n\
               echo 'Login failed: invalid authorization code' >&2\n\
             done\n",
        )
        .unwrap();
        std::fs::set_permissions(&program, PermissionsExt::from_mode(0o755)).unwrap();

        let s = LoginSessions::default();
        let node = Uuid::new_v4();
        let (session, _) = s
            .begin(node, &program.display().to_string(), &dir.join("creds"))
            .await
            .unwrap();

        let started = Instant::now();
        let err = s
            .complete(node, Some(session), "not-a-real-code")
            .await
            .unwrap_err();

        // Seconds, not the full timeout: the answer is in the output, and
        // waiting for an exit that is not coming is the bug.
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "a rejected code must answer promptly, took {:?}",
            started.elapsed()
        );
        match err {
            LoginError::Rejected(why) => assert!(
                why.contains("invalid authorization code"),
                "the operator needs the CLI's reason: {why}"
            ),
            other => panic!("expected a rejection, got {other}"),
        }
        // The child must not be left running once we have answered.
        assert_eq!(s.len().await, 0);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The race itself, forced rather than hoped for.
    ///
    /// QA's gate caught `wait_for_url` reporting a dead child's output before
    /// the readers had run, so the operator got "the CLI rejected it without
    /// saying why" instead of the reason. Through `begin` that window is a
    /// scheduling accident I could not reproduce. Here it is constructed: the
    /// child is confirmed DEAD first, the readers are spawned afterwards, and
    /// `wait_for_url` is called with no await in between — so on the
    /// single-threaded test runtime the readers cannot have run, and the
    /// output buffer is empty exactly as it was in the failure.
    #[tokio::test]
    async fn a_dead_childs_reason_is_read_before_it_is_reported() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!(
            "wheel-oauth-race-{}-{}",
            std::process::id(),
            Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let program = dir.join("stub");
        std::fs::write(&program, "#!/bin/sh\necho 'the real reason' >&2\nexit 1\n").unwrap();
        std::fs::set_permissions(&program, PermissionsExt::from_mode(0o755)).unwrap();

        let mut child = tokio::process::Command::new(&program)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();

        // Confirm it is gone. Its output is now sitting in the pipe buffer,
        // unread, because nothing is reading yet.
        loop {
            if matches!(child.try_wait(), Ok(Some(_))) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        let output = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let mut pumps = spawn_pumps(
            vec![
                Box::new(stdout) as Box<dyn tokio::io::AsyncRead + Unpin + Send>,
                Box::new(stderr) as Box<dyn tokio::io::AsyncRead + Unpin + Send>,
            ],
            &output,
        );
        assert!(
            output.lock().unwrap().is_empty(),
            "the readers must not have run yet, or this proves nothing"
        );

        let err = wait_for_url(&output, &mut child, &mut pumps, Duration::from_secs(5))
            .await
            .unwrap_err()
            .to_string();

        assert!(err.contains("ended before printing a URL"), "{err}");
        assert!(
            err.contains("the real reason"),
            "the child's own words must be read before they are reported: {err}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A stale browser tab must not finish a login the user already restarted.
    #[tokio::test]
    async fn a_code_for_a_superseded_session_is_refused() {
        let (program, dir) = stub_cli("stale", true);
        let creds = dir.join("creds");
        let s = LoginSessions::default();
        let node = Uuid::new_v4();

        let (first, _) = s.begin(node, &program, &creds).await.unwrap();
        let (second, _) = s.begin(node, &program, &creds).await.unwrap();
        assert_ne!(first, second);
        assert_eq!(s.len().await, 1, "the first login must have been killed");

        let err = s
            .complete(node, Some(first), "good#abc123")
            .await
            .unwrap_err();
        assert!(matches!(err, LoginError::Expired), "{err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The leak this exists to prevent: a user who signs in halfway and
    /// closes the tab. Nothing else ever calls `begin` again for that node,
    /// so without an armed timer the `claude auth login` child would sit in
    /// the sandbox forever.
    #[tokio::test]
    async fn an_abandoned_login_is_collected_when_its_ttl_runs_out() {
        let (program, dir) = stub_cli("abandoned", true);
        let creds = dir.join("creds");
        let s = LoginSessions::with_ttl(Duration::from_millis(150));
        let node = Uuid::new_v4();

        let (session, _) = s.begin(node, &program, &creds).await.unwrap();
        assert_eq!(s.len().await, 1);

        tokio::time::sleep(Duration::from_millis(500)).await;
        assert_eq!(
            s.len().await,
            0,
            "an abandoned login must not hold a child process forever"
        );
        // And the handle is genuinely gone, not merely unreachable.
        assert!(matches!(
            s.complete(node, Some(session), "good#abc123")
                .await
                .unwrap_err(),
            LoginError::NoSession
        ));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The timer is armed per login, so a retry inherits an older login's
    /// pending timer. That timer must not reap the replacement.
    #[tokio::test]
    async fn an_expiring_timer_does_not_kill_the_login_that_replaced_it() {
        let (program, dir) = stub_cli("replaced", true);
        let creds = dir.join("creds");
        let s = LoginSessions::with_ttl(Duration::from_millis(300));
        let node = Uuid::new_v4();

        s.begin(node, &program, &creds).await.unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        // Retry: the first login's timer is still pending and fires at ~300ms.
        let (second, _) = s.begin(node, &program, &creds).await.unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;

        assert_eq!(
            s.len().await,
            1,
            "the first login's timer reaped the retry that replaced it"
        );
        s.complete(node, Some(second), "good#abc123").await.unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn cancelling_kills_the_child_and_is_idempotent() {
        let (program, dir) = stub_cli("cancel", true);
        let creds = dir.join("creds");
        let s = LoginSessions::default();
        let node = Uuid::new_v4();

        s.begin(node, &program, &creds).await.unwrap();
        s.cancel(node).await;
        assert_eq!(s.len().await, 0);
        s.cancel(node).await;
        assert!(matches!(
            s.complete(node, None, "good#abc123").await.unwrap_err(),
            LoginError::NoSession
        ));
        std::fs::remove_dir_all(&dir).ok();
    }
}
