// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

//! Auto-update for wheeld: check, notice, request, apply, roll back
//! (docs/proposals/auto-update.md).
//!
//! The updater never kills a turn. It builds first, drains second, and only once
//! no agent is mid-turn does it hand the daemon a build to install and restart
//! onto. A drain that runs out of time resumes delivery and says so.

pub mod boot;
pub mod ci;
pub mod components;
pub mod driver;
pub mod policy;
pub mod state;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock, Weak};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use async_trait::async_trait;
use uuid::Uuid;
use wheel_core::{BlockReason, Component, Sha, UpdateNotice, UpdateState};
use wheel_engine::update::{EngineControl, RequestOutcome, UpdateHook};

use ci::{CiGate, Verdict};
use driver::{Relation, Staged, UpdateDriver};
pub use policy::{Mode, Policy};
use state::{Outcome, Pending, Request, Requester, Row, StateStore};

/// How often the daemon looks for requests and due fetches. Cheap: a stat and
/// a clock read unless a fetch is due.
pub const TICK: Duration = Duration::from_secs(15);
const DRAIN_POLL: Duration = Duration::from_millis(250);
/// A requester who never comes back (a deleted project) stops being retried.
const NOTIFY_FOR_SECS: u64 = 24 * 60 * 60;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// No check has finished since boot.
    Unknown,
    /// Current, not pertinent, or suppressed after a rollback: nothing to say.
    UpToDate,
    Candidate(Candidate),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub target: String,
    /// What a notice names: the components this deployment runs, plus `ci`.
    pub components: Vec<Component>,
    pub commits: u32,
    pub touches_ci: bool,
    /// Why it cannot apply for anyone, operator included.
    pub block: Option<BlockReason>,
}

pub trait Clock: Send + Sync {
    fn now(&self) -> u64;
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> u64 {
        state::now()
    }
}

/// What the daemon lends the updater: every engine it runs.
///
/// There is no "stop the agents" here: the daemon's own shutdown does that
/// (`Supervisor::shutdown`, from `api/headless-first`), and the updater's job
/// is to make that shutdown land on a board where no turn is running.
#[async_trait]
pub trait Runtime: Send + Sync {
    async fn pause(&self);
    async fn busy(&self) -> Vec<String>;
    async fn resume(&self);
    /// Tell whoever asked. `false` means try again later.
    async fn notify(&self, to: &Requester, body: String) -> bool;
}

/// A build that passed every gate, with the board drained and parked. The
/// daemon installs it once it has shut down.
#[derive(Debug)]
pub struct Ready {
    pub by: Requester,
    pub candidate: Candidate,
    pub staged: Staged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootAction {
    Run,
    /// The new binary on probation: prove healthy within `health_for`.
    Probation,
    /// Rolled back; restart onto the previous binary now.
    Restart,
}

pub struct Updater {
    policy: Policy,
    running: String,
    driver: Arc<dyn UpdateDriver>,
    ci: Arc<dyn CiGate>,
    store: StateStore,
    clock: Arc<dyn Clock>,
    status: RwLock<Status>,
    last_fetch: Mutex<Option<u64>>,
    checking: AtomicBool,
    applying: AtomicBool,
    wake: tokio::sync::Notify,
}

impl Updater {
    pub fn new(
        policy: Policy,
        running: String,
        driver: Arc<dyn UpdateDriver>,
        ci: Arc<dyn CiGate>,
        store: StateStore,
        clock: Arc<dyn Clock>,
    ) -> Arc<Self> {
        Arc::new(Self {
            policy,
            running,
            driver,
            ci,
            store,
            clock,
            status: RwLock::new(Status::Unknown),
            last_fetch: Mutex::new(None),
            checking: AtomicBool::new(false),
            applying: AtomicBool::new(false),
            wake: tokio::sync::Notify::new(),
        })
    }

    pub fn policy(&self) -> &Policy {
        &self.policy
    }

    pub fn running(&self) -> &str {
        &self.running
    }

    pub fn status(&self) -> Status {
        self.status.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn set_status(&self, status: Status) {
        *self.status.write().unwrap_or_else(|e| e.into_inner()) = status;
    }

    pub fn notice(&self) -> Option<UpdateNotice> {
        let Status::Candidate(c) = self.status() else {
            return None;
        };
        let (state, reason) = self.state_of(&c);
        Some(UpdateNotice {
            state,
            running: Sha::parse(&self.running)?,
            target: Sha::parse(&c.target)?,
            components: c.components.clone(),
            commits: c.commits,
            reason,
        })
    }

    fn state_of(&self, c: &Candidate) -> (UpdateState, Option<BlockReason>) {
        if self.applying.load(Ordering::SeqCst) {
            return (UpdateState::Applying, None);
        }
        if let Some(b) = c.block {
            return (UpdateState::Blocked, Some(b));
        }
        let now = self.clock.now();
        let (request, suspended, timed_out) = self.store.read(|s| {
            let timed_out = s.history.last().is_some_and(|r| {
                r.to == c.target && r.outcome == Outcome::DrainTimedOut && s.request.is_some()
            });
            (s.request.clone(), s.suspended(now), timed_out)
        });
        let by_operator = request.as_ref().is_some_and(|r| r.by == Requester::Operator);
        if suspended && !by_operator {
            return (UpdateState::Blocked, Some(BlockReason::Suspended));
        }
        if c.touches_ci && !by_operator {
            return (UpdateState::Blocked, Some(BlockReason::CiDefinitionChanged));
        }
        if timed_out {
            return (UpdateState::Blocked, Some(BlockReason::DrainTimedOut));
        }
        if request.is_some() {
            return (UpdateState::Requested, None);
        }
        match self.policy.mode {
            Mode::Auto => (UpdateState::Scheduled, None),
            _ => (UpdateState::Available, None),
        }
    }

    /// The lazy path: a CLI call starts a fetch if one is due, and never waits.
    pub fn poke(self: &Arc<Self>) {
        let Ok(rt) = tokio::runtime::Handle::try_current() else {
            return;
        };
        if !self.fetch_due() {
            return;
        }
        let me = self.clone();
        rt.spawn(async move {
            me.check().await;
        });
    }

    /// At most one fetch per `fetch_every`, however many callers ask.
    fn fetch_due(&self) -> bool {
        if self.checking.load(Ordering::SeqCst) {
            return false;
        }
        let now = self.clock.now();
        let mut last = self.last_fetch.lock().unwrap_or_else(|e| e.into_inner());
        if last.is_some_and(|t| now.saturating_sub(t) < self.policy.fetch_every.as_secs()) {
            return false;
        }
        *last = Some(now);
        true
    }

    /// Fetch, compare, gate, and publish. One at a time: a second caller gets
    /// the last answer rather than a second fetch.
    pub async fn check(&self) -> Status {
        if self.checking.swap(true, Ordering::SeqCst) {
            return self.status();
        }
        let status = match self.look().await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %format_args!("{e:#}"), "the update check failed; keeping the last answer");
                self.status()
            }
        };
        self.set_status(status.clone());
        let (notice, now) = (self.notice(), self.clock.now());
        let _ = self.store.update(|s| {
            s.last_notice = notice;
            s.checked_at = Some(now);
        });
        self.checking.store(false, Ordering::SeqCst);
        status
    }

    async fn look(&self) -> Result<Status> {
        let driver = self.driver.clone();
        let running = self.running.clone();
        let found = tokio::task::spawn_blocking(move || -> Result<_> {
            driver.fetch()?;
            let target = driver.target()?;
            if target == running {
                return Ok(None);
            }
            let inspection = driver.inspect(&running, &target)?;
            Ok(Some((target, inspection)))
        })
        .await
        .context("the update check panicked")??;

        let Some((target, inspection)) = found else {
            return Ok(Status::UpToDate);
        };
        if inspection.relation == Relation::Current || self.store.read(|s| s.is_bad(&target)) {
            return Ok(Status::UpToDate);
        }
        let changed = components::of(&inspection.paths);
        let fast_forward = inspection.relation == Relation::FastForward;
        if fast_forward && !components::pertinent(&changed, components::WHEELD_RUNS) {
            return Ok(Status::UpToDate);
        }

        let block = if !fast_forward {
            Some(BlockReason::NotFastForward)
        } else if !inspection.clean {
            Some(BlockReason::DirtyCheckout)
        } else {
            match self.ci.verdict(&target).await {
                Verdict::Green => None,
                Verdict::Pending => Some(BlockReason::CiPending),
                Verdict::Red(why) => {
                    tracing::info!(%target, %why, "CI is red on the update target");
                    Some(BlockReason::CiFailed)
                }
                Verdict::Unverifiable(why) => {
                    tracing::error!(%target, %why, "the CI gate cannot be verified, so nothing will be applied");
                    Some(BlockReason::CiUnverifiable)
                }
            }
        };
        Ok(Status::Candidate(Candidate {
            target,
            components: components::named(&changed, components::WHEELD_RUNS),
            commits: inspection.commits,
            touches_ci: changed.contains(&Component::Ci),
            block,
        }))
    }

    /// Record a request. Returns at once: the requester is mid-turn.
    pub fn request(&self, by: Requester) -> RequestOutcome {
        let candidate = match self.status() {
            Status::UpToDate => return RequestOutcome::NothingToDo,
            Status::Unknown => None,
            Status::Candidate(c) => Some(c),
        };
        let now = self.clock.now();
        if by != Requester::Operator {
            if self.store.read(|s| s.suspended(now)) {
                return RequestOutcome::Refused(BlockReason::Suspended.explain().into());
            }
            if let Some(c) = &candidate {
                let final_block = c.block.filter(|b| {
                    matches!(b, BlockReason::NotFastForward | BlockReason::DirtyCheckout)
                });
                let refusal = final_block.or(c.touches_ci.then_some(BlockReason::CiDefinitionChanged));
                if let Some(b) = refusal {
                    return RequestOutcome::Refused(b.explain().into());
                }
            }
        }
        let recorded = self.store.update(|s| match &s.request {
            Some(existing) if existing.by == Requester::Operator || by != Requester::Operator => false,
            _ => {
                s.request = Some(Request { by, at: now });
                true
            }
        });
        match recorded {
            Err(e) => RequestOutcome::Refused(format!("could not record the request: {e:#}")),
            Ok(fresh) => {
                self.wake.notify_one();
                if fresh {
                    RequestOutcome::Accepted(self.notice())
                } else {
                    RequestOutcome::AlreadyRequested(self.notice())
                }
            }
        }
    }

    /// `wheeld update` writes a file; the daemon takes it here. The operator
    /// outranks an agent's request and resets the breaker.
    fn take_operator_request(&self) {
        if let Some(at) = self.store.take_operator_request() {
            let _ = self.store.update(|s| {
                s.breaker_reset = Some(at);
                s.request = Some(Request {
                    by: Requester::Operator,
                    at,
                });
            });
            tracing::info!("the operator asked for an update");
        }
    }

    /// The daemon's loop: tick now, then every [`TICK`] or when a request wakes it.
    pub async fn run(self: Arc<Self>, rt: Arc<dyn Runtime>, restart: tokio::sync::oneshot::Sender<Ready>) {
        loop {
            if let Some(ready) = self.tick(rt.as_ref()).await {
                let _ = restart.send(ready);
                return;
            }
            tokio::select! {
                _ = tokio::time::sleep(TICK) => {}
                _ = self.wake.notified() => {}
            }
        }
    }

    pub async fn tick(&self, rt: &dyn Runtime) -> Option<Ready> {
        self.take_operator_request();
        if self.fetch_due() {
            self.check().await;
        }
        self.notify_outstanding(rt).await;
        let by = self.due(rt).await?;
        self.apply(by, rt).await
    }

    /// Who an apply would be for right now, if anyone.
    async fn due(&self, rt: &dyn Runtime) -> Option<Requester> {
        let Status::Candidate(c) = self.status() else {
            if self.status() == Status::UpToDate {
                let _ = self.store.update(|s| s.request = None);
            }
            return None;
        };
        let request = self.store.read(|s| s.request.clone());
        let by = match request {
            Some(r) => r.by,
            None if self.policy.mode == Mode::Auto => Requester::Auto,
            None => return None,
        };
        if c.block.is_some() {
            return None;
        }
        if by != Requester::Operator {
            let now = self.clock.now();
            let (suspended, last) = self.store.read(|s| (s.suspended(now), s.last_attempt));
            let cooling = last.is_some_and(|t| now.saturating_sub(t) < self.policy.cooldown.as_secs());
            if suspended || cooling || c.touches_ci {
                return None;
            }
        }
        if by == Requester::Auto && !rt.busy().await.is_empty() {
            return None;
        }
        Some(by)
    }

    /// Steps 1-6: verify, build, smoke, drain, park. Returns the build to
    /// install, or `None` if this attempt ended here (and says why).
    pub async fn apply(&self, by: Requester, rt: &dyn Runtime) -> Option<Ready> {
        if self.applying.swap(true, Ordering::SeqCst) {
            return None;
        }
        let ready = self.attempt(&by, rt).await;
        if ready.is_none() {
            self.applying.store(false, Ordering::SeqCst);
        }
        ready
    }

    async fn attempt(&self, by: &Requester, rt: &dyn Runtime) -> Option<Ready> {
        let now = self.clock.now();
        let _ = self.store.update(|s| s.last_attempt = Some(now));

        let Status::Candidate(c) = self.check().await else {
            let _ = self.store.update(|s| s.request = None);
            return None;
        };
        let refusal = c.block.or((c.touches_ci && *by != Requester::Operator)
            .then_some(BlockReason::CiDefinitionChanged));
        if let Some(reason) = refusal {
            self.finish(by, &c, Outcome::Refused(reason), Vec::new(), rt).await;
            return None;
        }

        let (driver, target) = (self.driver.clone(), c.target.clone());
        let built = tokio::task::spawn_blocking(move || {
            let staged = driver.build(&target).map_err(|e| (Outcome::BuildFailed, e))?;
            driver
                .smoke(&staged, &target)
                .map_err(|e| (Outcome::SmokeFailed, e))?;
            Ok::<_, (Outcome, anyhow::Error)>(staged)
        })
        .await;
        let staged = match built {
            Ok(Ok(staged)) => staged,
            Ok(Err((outcome, e))) => {
                tracing::error!(target = %c.target, ?outcome, error = %format_args!("{e:#}"), "the update did not build");
                self.finish(by, &c, outcome, Vec::new(), rt).await;
                return None;
            }
            Err(e) => {
                tracing::error!(error = %e, "the update build panicked");
                self.finish(by, &c, Outcome::BuildFailed, Vec::new(), rt).await;
                return None;
            }
        };

        rt.pause().await;
        let deadline = Instant::now() + self.policy.drain_for;
        loop {
            let busy = rt.busy().await;
            if busy.is_empty() {
                break;
            }
            if Instant::now() >= deadline {
                rt.resume().await;
                tracing::warn!(target = %c.target, busy = %busy.join(", "), "drain timed out; delivery resumed");
                self.finish(by, &c, Outcome::DrainTimedOut, busy, rt).await;
                return None;
            }
            tokio::time::sleep(DRAIN_POLL).await;
        }
        tracing::info!(target = %c.target, "the board is quiet; handing the update to the daemon");
        Some(Ready {
            by: by.clone(),
            candidate: c,
            staged,
        })
    }

    /// Record an attempt that ended before a restart, and tell whoever asked.
    async fn finish(
        &self,
        by: &Requester,
        c: &Candidate,
        outcome: Outcome,
        busy: Vec<String>,
        rt: &dyn Runtime,
    ) {
        let keep_request = matches!(
            outcome,
            Outcome::DrainTimedOut | Outcome::Refused(BlockReason::CiPending)
        );
        let mut row = Row {
            at: self.clock.now(),
            from: self.running.clone(),
            to: c.target.clone(),
            by: by.clone(),
            outcome,
            components: c.components.clone(),
            busy,
            notified: false,
        };
        row.notified = rt.notify(by, message(&row)).await;
        let _ = self.store.update(|s| {
            s.record(row);
            if !keep_request {
                s.request = None;
            }
        });
    }

    /// Step 7, after the daemon has shut down: the marker first, then the swap.
    /// A crash between the two leaves a marker the next boot reads as
    /// interrupted, never an unrecorded binary.
    pub fn install(&self, ready: &Ready) -> Result<()> {
        let c = &ready.candidate;
        let now = self.clock.now();
        self.store.update(|s| {
            s.pending = Some(Pending {
                from: self.running.clone(),
                to: c.target.clone(),
                by: ready.by.clone(),
                components: c.components.clone(),
                attempts: 0,
                at: now,
            });
            s.request = None;
        })?;
        if let Err(e) = self.driver.swap(&ready.staged) {
            let _ = self.driver.rollback();
            self.store.update(|s| {
                s.pending = None;
                s.record(Row {
                    at: now,
                    from: self.running.clone(),
                    to: c.target.clone(),
                    by: ready.by.clone(),
                    outcome: Outcome::SwapFailed,
                    components: c.components.clone(),
                    busy: Vec::new(),
                    notified: false,
                });
            })?;
            return Err(e);
        }
        Ok(())
    }

    /// Step 9, before anything else runs on boot.
    pub fn settle_boot(&self) -> Result<BootAction> {
        let pending = self.store.read(|s| s.pending.clone());
        match boot::decide(pending.as_ref(), &self.running) {
            boot::Boot::Normal => Ok(BootAction::Run),
            boot::Boot::Interrupted(p) => {
                self.close(&p, Outcome::Interrupted)?;
                Ok(BootAction::Run)
            }
            boot::Boot::RollBack(p) => {
                tracing::error!(to = %p.to, "the new binary never proved healthy; rolling back");
                self.roll_back(&p)?;
                Ok(BootAction::Restart)
            }
            boot::Boot::Probation(_) => {
                self.store.update(|s| {
                    if let Some(p) = s.pending.as_mut() {
                        p.attempts += 1;
                    }
                })?;
                Ok(BootAction::Probation)
            }
        }
    }

    /// Put the previous binaries back and never offer this SHA again.
    pub fn roll_back(&self, p: &Pending) -> Result<()> {
        self.driver.rollback()?;
        self.store.update(|s| s.mark_bad(&p.to))?;
        self.close(p, Outcome::RolledBack)
    }

    /// The new binary proved healthy: keep it, and make the checkout match.
    pub fn confirm(&self) -> Result<()> {
        let Some(p) = self.store.read(|s| s.pending.clone()) else {
            return Ok(());
        };
        self.close(&p, Outcome::Updated)?;
        if let Err(e) = self.driver.settle(&p.to) {
            tracing::warn!(error = %format_args!("{e:#}"), "updated, but could not fast-forward the checkout");
        }
        tracing::info!(from = %p.from, to = %p.to, "updated");
        Ok(())
    }

    fn close(&self, p: &Pending, outcome: Outcome) -> Result<()> {
        let now = self.clock.now();
        self.store.update(|s| {
            s.pending = None;
            s.record(Row {
                at: now,
                from: p.from.clone(),
                to: p.to.clone(),
                by: p.by.clone(),
                outcome,
                components: p.components.clone(),
                busy: Vec::new(),
                notified: false,
            });
        })
    }

    /// Rows whose requester has not been told yet — the requester of a restart
    /// is told by the binary that comes up afterwards.
    async fn notify_outstanding(&self, rt: &dyn Runtime) {
        let now = self.clock.now();
        let rows: Vec<Row> = self
            .store
            .read(|s| s.history.iter().filter(|r| !r.notified).cloned().collect());
        for row in rows {
            let told = now.saturating_sub(row.at) > NOTIFY_FOR_SECS || rt.notify(&row.by, message(&row)).await;
            if told {
                let _ = self.store.update(|s| {
                    for r in s.history.iter_mut().filter(|r| **r == row) {
                        r.notified = true;
                    }
                });
            }
        }
    }
}

/// What the requester reads, from fixed words, hex and node names only: the
/// same no-git-text rule as the notice.
pub fn message(row: &Row) -> String {
    let short = |s: &str| s.chars().take(7).collect::<String>();
    let span = format!("{}→{}", short(&row.from), short(&row.to));
    let what: Vec<&str> = row.components.iter().map(|c| c.as_str()).collect();
    let what = if what.is_empty() {
        String::new()
    } else {
        format!(" ({})", what.join(", "))
    };
    let not_applied = format!("wheel runtime update {span}{what} was not applied");
    match &row.outcome {
        Outcome::Updated => format!(
            "wheel runtime updated {span}{what}. The board restarted while no turn was running; \
             your session resumed where it was."
        ),
        Outcome::RolledBack => format!(
            "wheel runtime update {span}{what} failed its health check and was rolled back; still \
             running {}. That commit will not be offered again.",
            short(&row.from)
        ),
        Outcome::DrainTimedOut => format!(
            "{not_applied}: {} still mid-turn when the drain window ran out. Delivery has resumed; \
             it will be retried.",
            if row.busy.is_empty() {
                "agents were".to_string()
            } else {
                row.busy.join(", ")
            }
        ),
        Outcome::Refused(reason) => format!("{not_applied}: {}.", reason.explain()),
        Outcome::BuildFailed => {
            format!("{not_applied}: it did not build (the operator can see why in wheeld's log).")
        }
        Outcome::SmokeFailed => format!("{not_applied}: the new binary failed its smoke test."),
        Outcome::SwapFailed => format!("{not_applied}: it could not be installed."),
        Outcome::Interrupted => format!("{not_applied}: it was interrupted before it was installed."),
    }
}

/// Every engine this daemon runs, held weakly: a stopped engine drops out.
#[derive(Default)]
pub struct Registry {
    engines: Mutex<HashMap<Uuid, Weak<dyn EngineControl>>>,
    paused: AtomicBool,
}

impl Registry {
    pub fn attach(&self, project: Uuid, engine: Weak<dyn EngineControl>) {
        // An engine that starts mid-drain joins the pause, or it would start
        // turns the shutdown then kills.
        if self.paused.load(Ordering::SeqCst) {
            if let Some(e) = engine.upgrade() {
                e.pause();
            }
        }
        self.engines
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(project, engine);
    }

    fn live(&self) -> Vec<(Uuid, Arc<dyn EngineControl>)> {
        let mut engines = self.engines.lock().unwrap_or_else(|e| e.into_inner());
        engines.retain(|_, e| e.strong_count() > 0);
        engines
            .iter()
            .filter_map(|(id, e)| e.upgrade().map(|e| (*id, e)))
            .collect()
    }
}

#[async_trait]
impl Runtime for Registry {
    async fn pause(&self) {
        self.paused.store(true, Ordering::SeqCst);
        for (_, e) in self.live() {
            e.pause();
        }
    }

    async fn busy(&self) -> Vec<String> {
        let mut out = Vec::new();
        for (_, e) in self.live() {
            out.extend(e.busy().await);
        }
        out
    }

    async fn resume(&self) {
        self.paused.store(false, Ordering::SeqCst);
        for (_, e) in self.live() {
            e.resume().await;
        }
    }

    async fn notify(&self, to: &Requester, body: String) -> bool {
        match to {
            Requester::Agent { project, node, .. } => {
                let engine = self
                    .live()
                    .into_iter()
                    .find(|(id, _)| id == project)
                    .map(|(_, e)| e);
                match engine {
                    Some(e) => e.post_system(*node, body).await,
                    None => false,
                }
            }
            Requester::Operator | Requester::Auto => {
                tracing::info!(target: "wheeld::update", "{body}");
                true
            }
        }
    }
}

/// What each engine holds: the updater's notice and request, and the registry
/// that lets the updater drain it.
pub struct Hook {
    pub updater: Arc<Updater>,
    pub registry: Arc<Registry>,
}

impl UpdateHook for Hook {
    fn notice(&self) -> Option<UpdateNotice> {
        self.updater.poke();
        self.updater.notice()
    }

    fn request(&self, from: wheel_engine::update::Requester) -> RequestOutcome {
        self.updater.request(Requester::Agent {
            project: from.project,
            node: from.node,
            name: from.name,
        })
    }

    fn attach(&self, project: Uuid, engine: Weak<dyn EngineControl>) {
        self.registry.attach(project, engine);
    }
}

/// The running build's SHA: stamped at compile time, or — for a first install
/// built without `WHEEL_BUILD_SHA` — the checkout's HEAD, said out loud.
pub fn running_sha(stamped: &str, driver: &dyn UpdateDriver) -> Result<String> {
    if stamped.len() == 40 && Sha::parse(stamped).is_some() {
        return Ok(stamped.to_string());
    }
    let head = driver
        .head()
        .context("this wheeld is unstamped and the checkout's HEAD cannot be read")?;
    tracing::warn!(
        %head,
        "this wheeld was built without WHEEL_BUILD_SHA; assuming it is the checkout's HEAD. \
         Every build the updater makes is stamped, so this is only true until the first update."
    );
    Ok(head)
}

#[cfg(test)]
mod tests;
