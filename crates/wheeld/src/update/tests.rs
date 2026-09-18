// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

//! The updater's decisions, against fakes: what it checks, what it refuses, the
//! order it does things in, and what it does when a step fails.
//!
//! The real git, cargo and GitHub halves are tested in `driver` and `ci`; this
//! is the part that decides whether any of them run.

use super::*;
use driver::{Inspection, Relation, Staged};
use std::sync::atomic::AtomicU64;

/// A clock a test moves by hand: a rate limit measured against the real one is
/// a test that either sleeps or lies.
struct ManualClock(AtomicU64);

impl Clock for ManualClock {
    fn now(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

#[derive(Default)]
struct FakeDriver {
    log: Mutex<Vec<String>>,
    target: Mutex<String>,
    inspection: Mutex<Option<Inspection>>,
    build_fails: AtomicBool,
    smoke_fails: AtomicBool,
    swap_fails: AtomicBool,
}

const RUNNING: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const TARGET: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

impl FakeDriver {
    fn new() -> Arc<Self> {
        let d = Arc::new(FakeDriver::default());
        *d.target.lock().unwrap() = TARGET.to_string();
        d.set(
            Relation::FastForward,
            vec!["crates/wheel-engine/src/lib.rs".into()],
        );
        d
    }

    fn set(&self, relation: Relation, paths: Vec<String>) {
        *self.inspection.lock().unwrap() = Some(Inspection {
            relation,
            clean: true,
            commits: 3,
            paths,
        });
    }

    fn dirty(&self) {
        if let Some(i) = self.inspection.lock().unwrap().as_mut() {
            i.clean = false;
        }
    }

    fn did(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }

    fn note(&self, what: &str) {
        self.log.lock().unwrap().push(what.to_string());
    }
}

impl UpdateDriver for FakeDriver {
    fn name(&self) -> &'static str {
        "fake"
    }
    fn head(&self) -> Result<String> {
        Ok(RUNNING.into())
    }
    fn fetch(&self) -> Result<()> {
        self.note("fetch");
        Ok(())
    }
    fn target(&self) -> Result<String> {
        Ok(self.target.lock().unwrap().clone())
    }
    fn inspect(&self, _running: &str, _target: &str) -> Result<Inspection> {
        Ok(self.inspection.lock().unwrap().clone().unwrap())
    }
    fn build(&self, _target: &str) -> Result<Staged> {
        self.note("build");
        if self.build_fails.load(Ordering::SeqCst) {
            anyhow::bail!("cargo said no");
        }
        Ok(Staged {
            dir: std::path::PathBuf::from("/tmp/staged"),
            binaries: vec![],
        })
    }
    fn smoke(&self, _staged: &Staged, _target: &str) -> Result<()> {
        self.note("smoke");
        if self.smoke_fails.load(Ordering::SeqCst) {
            anyhow::bail!("the new binary does not run");
        }
        Ok(())
    }
    fn swap(&self, _staged: &Staged) -> Result<()> {
        self.note("swap");
        if self.swap_fails.load(Ordering::SeqCst) {
            anyhow::bail!("read-only bin dir");
        }
        Ok(())
    }
    fn rollback(&self) -> Result<()> {
        self.note("rollback");
        Ok(())
    }
    fn settle(&self, _target: &str) -> Result<()> {
        self.note("settle");
        Ok(())
    }
}

struct FakeCi(Mutex<Verdict>);

#[async_trait]
impl CiGate for FakeCi {
    async fn verdict(&self, _sha: &str) -> Verdict {
        self.0.lock().unwrap().clone()
    }
}

#[derive(Default)]
struct FakeRuntime {
    log: Mutex<Vec<String>>,
    /// Answers for successive `busy` calls; the last one repeats.
    busy: Mutex<Vec<Vec<String>>>,
    notices: Mutex<Vec<(Requester, String)>>,
    deaf: AtomicBool,
}

impl FakeRuntime {
    fn quiet() -> Arc<Self> {
        Arc::new(FakeRuntime::default())
    }

    fn busy_until_quiet(turns: usize) -> Arc<Self> {
        let rt = FakeRuntime::default();
        let mut answers = vec![vec!["pm".to_string()]; turns];
        answers.push(vec![]);
        *rt.busy.lock().unwrap() = answers;
        Arc::new(rt)
    }

    fn never_quiet() -> Arc<Self> {
        let rt = FakeRuntime::default();
        *rt.busy.lock().unwrap() = vec![vec!["pm".to_string()]];
        Arc::new(rt)
    }

    fn did(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }
}

#[async_trait]
impl Runtime for FakeRuntime {
    async fn pause(&self) {
        self.log.lock().unwrap().push("pause".into());
    }
    async fn busy(&self) -> Vec<String> {
        self.log.lock().unwrap().push("busy".into());
        let mut queue = self.busy.lock().unwrap();
        if queue.len() > 1 {
            queue.remove(0)
        } else {
            queue.first().cloned().unwrap_or_default()
        }
    }
    async fn resume(&self) {
        self.log.lock().unwrap().push("resume".into());
    }
    async fn notify(&self, to: &Requester, body: String) -> bool {
        if self.deaf.load(Ordering::SeqCst) {
            return false;
        }
        self.notices.lock().unwrap().push((to.clone(), body));
        true
    }
}

struct Fixture {
    updater: Arc<Updater>,
    driver: Arc<FakeDriver>,
    ci: Arc<FakeCi>,
    clock: Arc<ManualClock>,
    dir: std::path::PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.dir).ok();
    }
}

fn fixture(mode: Mode) -> Fixture {
    let dir = std::env::temp_dir().join(format!("wheeld-upd-{}", Uuid::new_v4()));
    let policy = Policy {
        mode,
        repo: dir.join("repo"),
        github_token: Some("t".into()),
        github_repo: Some("o/r".into()),
        required_checks: vec!["make check".into()],
        bin_dir: dir.join("bin"),
        staging: dir.join("staging"),
        fetch_every: Duration::from_secs(300),
        drain_for: Duration::from_millis(600),
        health_for: Duration::from_secs(60),
        cooldown: Duration::from_secs(600),
        restart: policy::Restart::Exec,
    };
    let driver = FakeDriver::new();
    let ci = Arc::new(FakeCi(Mutex::new(Verdict::Green)));
    let clock = Arc::new(ManualClock(AtomicU64::new(1_000_000)));
    let updater = Updater::new(
        policy,
        RUNNING.to_string(),
        driver.clone(),
        ci.clone(),
        StateStore::open(&dir.join("update")).unwrap(),
        clock.clone(),
    );
    Fixture {
        updater,
        driver,
        ci,
        clock,
        dir,
    }
}

fn agent() -> Requester {
    Requester::Agent {
        project: Uuid::nil(),
        node: Uuid::nil(),
        name: "pm".into(),
    }
}

#[tokio::test]
async fn a_pertinent_green_commit_is_offered_to_agents() {
    let f = fixture(Mode::Prompt);
    f.updater.check().await;
    let notice = f.updater.notice().expect("a notice");
    assert_eq!(notice.state, UpdateState::Available);
    assert_eq!(notice.target.as_str(), TARGET);
    assert_eq!(notice.components, vec![Component::Engine]);
    assert!(
        notice.line().contains("run `wheel update`"),
        "{}",
        notice.line()
    );

    // In `auto` the agent is told it needs to do nothing.
    let f = fixture(Mode::Auto);
    f.updater.check().await;
    assert_eq!(f.updater.notice().unwrap().state, UpdateState::Scheduled);
}

#[tokio::test]
async fn a_docs_only_change_is_not_worth_a_restart() {
    let f = fixture(Mode::Prompt);
    f.driver.set(
        Relation::FastForward,
        vec!["docs/a.md".into(), "web/x.ts".into()],
    );
    assert_eq!(f.updater.check().await, Status::UpToDate);
    assert_eq!(f.updater.notice(), None);
    assert_eq!(f.updater.request(agent()), RequestOutcome::NothingToDo);
}

/// The gate that stops a board deploying a commit CI has not blessed.
#[tokio::test]
async fn nothing_applies_unless_ci_is_green_and_unverifiable_is_never_green() {
    for (verdict, reason) in [
        (Verdict::Pending, BlockReason::CiPending),
        (
            Verdict::Red("failed: make check".into()),
            BlockReason::CiFailed,
        ),
        (
            Verdict::Unverifiable("no token".into()),
            BlockReason::CiUnverifiable,
        ),
    ] {
        let f = fixture(Mode::Auto);
        *f.ci.0.lock().unwrap() = verdict.clone();
        f.updater.check().await;
        let notice = f.updater.notice().expect("still offered, with the reason");
        assert_eq!(notice.state, UpdateState::Blocked, "{verdict:?}");
        assert_eq!(notice.reason, Some(reason), "{verdict:?}");

        let rt = FakeRuntime::quiet();
        assert!(f.updater.tick(rt.as_ref()).await.is_none(), "{verdict:?}");
        assert!(
            !f.driver.did().contains(&"build".to_string()),
            "{verdict:?}"
        );
    }
}

/// Fast-forward only, and never over the operator's own work.
#[tokio::test]
async fn a_diverged_or_dirty_checkout_is_refused_not_merged_over() {
    let f = fixture(Mode::Auto);
    f.driver
        .set(Relation::Diverged, vec!["crates/wheel-engine/a.rs".into()]);
    f.updater.check().await;
    assert_eq!(
        f.updater.notice().unwrap().reason,
        Some(BlockReason::NotFastForward)
    );
    let rt = FakeRuntime::quiet();
    assert!(f.updater.tick(rt.as_ref()).await.is_none());
    assert!(matches!(
        f.updater.request(agent()),
        RequestOutcome::Refused(_)
    ));

    let f = fixture(Mode::Auto);
    f.driver.dirty();
    f.updater.check().await;
    assert_eq!(
        f.updater.notice().unwrap().reason,
        Some(BlockReason::DirtyCheckout)
    );
    assert!(f
        .updater
        .tick(FakeRuntime::quiet().as_ref())
        .await
        .is_none());
}

#[tokio::test]
async fn a_request_is_recorded_once_and_answered_at_once() {
    let f = fixture(Mode::Prompt);
    f.updater.check().await;

    let first = f.updater.request(agent());
    assert!(
        matches!(first, RequestOutcome::Accepted(Some(_))),
        "{first:?}"
    );
    assert_eq!(
        f.updater.notice().unwrap().state,
        UpdateState::Requested,
        "every agent now sees that it is coming"
    );
    assert!(matches!(
        f.updater.request(agent()),
        RequestOutcome::AlreadyRequested(_)
    ));
    let state = state::peek(&f.dir.join("update"));
    assert_eq!(state.request.map(|r| r.by), Some(agent()));
}

/// T1b: CI is defined by the commit it gates, so a range that rewrites CI is
/// the operator's call, never an agent's and never `auto`'s.
#[tokio::test]
async fn a_range_that_changes_ci_is_operator_only() {
    let f = fixture(Mode::Auto);
    f.driver.set(
        Relation::FastForward,
        vec![
            ".github/workflows/ci.yml".into(),
            "crates/wheel-engine/src/lib.rs".into(),
        ],
    );
    f.updater.check().await;
    let notice = f.updater.notice().unwrap();
    assert_eq!(notice.reason, Some(BlockReason::CiDefinitionChanged));
    assert!(
        notice.components.contains(&Component::Ci),
        "the notice says why"
    );

    assert!(matches!(
        f.updater.request(agent()),
        RequestOutcome::Refused(_)
    ));
    assert!(
        f.updater
            .tick(FakeRuntime::quiet().as_ref())
            .await
            .is_none(),
        "auto must not apply it either"
    );

    // The operator may, and then it applies.
    assert!(matches!(
        f.updater.request(Requester::Operator),
        RequestOutcome::Accepted(_)
    ));
    assert!(f
        .updater
        .tick(FakeRuntime::quiet().as_ref())
        .await
        .is_some());
}

/// The order is the safety argument: build before anyone is paused, pause
/// before waiting, and only hand over a build once nothing is mid-turn.
#[tokio::test]
async fn an_apply_builds_first_then_pauses_then_waits_for_quiet() {
    let f = fixture(Mode::Prompt);
    f.updater.check().await;
    f.updater.request(agent());
    let rt = FakeRuntime::busy_until_quiet(2);

    let ready = f
        .updater
        .tick(rt.as_ref())
        .await
        .expect("a build to install");
    assert_eq!(ready.candidate.target, TARGET);
    assert_eq!(f.driver.did(), vec!["fetch", "fetch", "build", "smoke"]);
    assert_eq!(
        rt.did(),
        vec!["pause", "busy", "busy", "busy"],
        "paused, then waited for the turns already running"
    );
    assert!(
        !f.driver.did().contains(&"swap".to_string()),
        "nothing is installed until the daemon has shut down"
    );
}

/// The ruling this feature rests on: an update never ends a turn. When the wait
/// runs out, the update is what gives up.
#[tokio::test]
async fn a_wait_that_runs_out_resumes_delivery_and_reports_who_was_busy() {
    let f = fixture(Mode::Prompt);
    f.updater.check().await;
    f.updater.request(agent());
    let rt = FakeRuntime::never_quiet();

    assert!(f.updater.tick(rt.as_ref()).await.is_none());
    let did = rt.did();
    assert_eq!(did.first().unwrap(), "pause");
    assert_eq!(did.last().unwrap(), "resume", "delivery must come back");

    let state = state::peek(&f.dir.join("update"));
    let row = state.history.last().expect("an attempt is recorded");
    assert_eq!(row.outcome, Outcome::DrainTimedOut);
    assert_eq!(row.busy, vec!["pm".to_string()]);
    assert!(
        state.request.is_some(),
        "the request stands: it is retried rather than forgotten"
    );
    let (to, body) = rt.notices.lock().unwrap().first().cloned().unwrap();
    assert_eq!(to, agent());
    assert!(
        body.contains("pm"),
        "the requester is told who was busy: {body}"
    );
    assert!(body.contains("Delivery has resumed"), "{body}");
}

#[tokio::test]
async fn a_build_or_smoke_failure_stops_the_attempt_and_tells_the_requester() {
    type Breaks = fn(&FakeDriver) -> &AtomicBool;
    let pick: [(Breaks, Outcome); 2] = [
        (|d| &d.build_fails, Outcome::BuildFailed),
        (|d| &d.smoke_fails, Outcome::SmokeFailed),
    ];
    for (fail, outcome) in pick {
        let f = fixture(Mode::Prompt);
        f.updater.check().await;
        f.updater.request(agent());
        fail(&f.driver).store(true, Ordering::SeqCst);
        let rt = FakeRuntime::quiet();

        assert!(f.updater.tick(rt.as_ref()).await.is_none());
        assert!(
            !rt.did().contains(&"pause".to_string()),
            "nobody was paused"
        );
        let state = state::peek(&f.dir.join("update"));
        assert_eq!(state.history.last().unwrap().outcome, outcome);
        assert!(!rt.notices.lock().unwrap().is_empty());
    }
}

/// A lazy check on a CLI call must not turn agent chatter into a fetch storm.
#[tokio::test]
async fn a_lazy_check_fetches_at_most_once_per_interval() {
    let f = fixture(Mode::Prompt);
    for _ in 0..10 {
        f.updater.poke();
    }
    for _ in 0..50 {
        if f.driver.did().len() == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(f.driver.did(), vec!["fetch"], "ten calls, one fetch");

    f.clock.0.fetch_add(299, Ordering::SeqCst);
    f.updater.poke();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(f.driver.did().len(), 1, "still inside the interval");

    f.clock.0.fetch_add(2, Ordering::SeqCst);
    f.updater.poke();
    for _ in 0..50 {
        if f.driver.did().len() == 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        f.driver.did().len(),
        2,
        "past the interval it fetches again"
    );
}

#[tokio::test]
async fn install_records_the_marker_before_it_swaps_and_undoes_a_failed_swap() {
    let f = fixture(Mode::Prompt);
    f.updater.check().await;
    let ready = Ready {
        by: agent(),
        candidate: match f.updater.status() {
            Status::Candidate(c) => c,
            other => panic!("{other:?}"),
        },
        staged: Staged {
            dir: std::path::PathBuf::from("/tmp/staged"),
            binaries: vec![],
        },
    };

    f.updater.install(&ready).unwrap();
    let state = state::peek(&f.dir.join("update"));
    let pending = state.pending.expect("a marker the next boot can read");
    assert_eq!(
        (pending.from.as_str(), pending.to.as_str()),
        (RUNNING, TARGET)
    );
    assert_eq!(pending.attempts, 0);
    assert_eq!(f.driver.did().last().unwrap(), "swap");

    // A swap that fails puts back what it replaced and leaves no marker claiming
    // a binary that was never installed.
    let f = fixture(Mode::Prompt);
    f.updater.check().await;
    f.driver.swap_fails.store(true, Ordering::SeqCst);
    assert!(f.updater.install(&ready).is_err());
    let state = state::peek(&f.dir.join("update"));
    assert!(state.pending.is_none());
    assert_eq!(state.history.last().unwrap().outcome, Outcome::SwapFailed);
    assert!(f.driver.did().contains(&"rollback".to_string()));
}

/// The health check is the whole rollback story: a build that does not come up
/// is put back, and never offered again.
#[tokio::test]
async fn a_build_that_never_proves_healthy_is_rolled_back_and_not_offered_again() {
    let f = fixture(Mode::Prompt);
    f.updater.check().await;
    assert!(f.updater.notice().is_some(), "offered before it failed");

    // What the watchdog runs when the new build has not answered in time.
    let pending = Pending {
        from: "c".repeat(40),
        to: TARGET.into(),
        by: agent(),
        components: vec![Component::Engine],
        attempts: 1,
        at: 0,
    };
    f.updater.store_pending_for_test(pending.clone());
    f.updater.roll_back(&pending).unwrap();

    assert!(f.driver.did().contains(&"rollback".to_string()));
    let state = state::peek(&f.dir.join("update"));
    assert!(
        state.pending.is_none(),
        "no marker claims a build that was put back"
    );
    assert!(state.bad.contains(&TARGET.to_string()));
    assert_eq!(state.history.last().unwrap().outcome, Outcome::RolledBack);

    // ...and that commit is never offered or applied again.
    assert_eq!(f.updater.check().await, Status::UpToDate);
    assert_eq!(f.updater.notice(), None);
    assert_eq!(f.updater.request(agent()), RequestOutcome::NothingToDo);
}

/// The crash case: a new build that died before it could confirm boots again,
/// and must put the old one back before it serves anything.
#[tokio::test]
async fn a_new_build_that_crashed_rolls_back_on_its_next_boot() {
    let f = fixture(Mode::Prompt);
    // This process IS the new build (`to` is what is running), and it has been
    // here before without confirming.
    f.updater.store_pending_for_test(Pending {
        from: "c".repeat(40),
        to: RUNNING.into(),
        by: agent(),
        components: vec![Component::Engine],
        attempts: 1,
        at: 0,
    });
    assert_eq!(f.updater.settle_boot().unwrap(), BootAction::Restart);
    assert!(f.driver.did().contains(&"rollback".to_string()));
    let state = state::peek(&f.dir.join("update"));
    assert!(state.bad.contains(&RUNNING.to_string()));
    assert_eq!(state.history.last().unwrap().outcome, Outcome::RolledBack);
}

#[tokio::test]
async fn a_first_boot_of_a_new_build_is_probation_and_confirming_keeps_it() {
    let f = fixture(Mode::Prompt);
    f.updater.store_pending_for_test(Pending {
        from: RUNNING.into(),
        to: RUNNING.into(),
        by: Requester::Operator,
        components: vec![Component::Engine],
        attempts: 0,
        at: 0,
    });
    assert_eq!(f.updater.settle_boot().unwrap(), BootAction::Probation);
    assert_eq!(
        state::peek(&f.dir.join("update")).pending.unwrap().attempts,
        1,
        "a second boot without a confirm is a crash"
    );

    f.updater.confirm().unwrap();
    let state = state::peek(&f.dir.join("update"));
    assert!(state.pending.is_none());
    assert_eq!(state.history.last().unwrap().outcome, Outcome::Updated);
    assert!(
        f.driver.did().contains(&"settle".to_string()),
        "the checkout is fast-forwarded to what now runs"
    );
}

/// A marker naming a binary this process is not means the swap never landed.
#[tokio::test]
async fn a_marker_for_a_binary_that_never_started_is_closed_as_interrupted() {
    let f = fixture(Mode::Prompt);
    f.updater.store_pending_for_test(Pending {
        from: RUNNING.into(),
        to: "c".repeat(40),
        by: agent(),
        components: vec![],
        attempts: 0,
        at: 0,
    });
    assert_eq!(f.updater.settle_boot().unwrap(), BootAction::Run);
    let state = state::peek(&f.dir.join("update"));
    assert!(state.pending.is_none());
    assert_eq!(state.history.last().unwrap().outcome, Outcome::Interrupted);
}

/// Repeated failure stops `auto` rather than restarting the board all day.
#[tokio::test]
async fn repeated_failures_suspend_auto_until_the_operator_returns() {
    let f = fixture(Mode::Auto);
    f.updater.check().await;
    f.driver.build_fails.store(true, Ordering::SeqCst);
    let rt = FakeRuntime::quiet();
    for _ in 0..state::BREAKER_FAILURES {
        f.updater.tick(rt.as_ref()).await;
        // Past the cooldown, so only the breaker can be what stops the next one.
        f.clock.0.fetch_add(10_000, Ordering::SeqCst);
    }
    let before = f.driver.did().iter().filter(|c| *c == "build").count();
    assert_eq!(before, state::BREAKER_FAILURES);

    f.updater.tick(rt.as_ref()).await;
    assert_eq!(
        f.driver.did().iter().filter(|c| *c == "build").count(),
        before,
        "auto is suspended, so nothing was built"
    );
    assert_eq!(
        f.updater.notice().unwrap().reason,
        Some(BlockReason::Suspended)
    );
    assert!(matches!(
        f.updater.request(agent()),
        RequestOutcome::Refused(_)
    ));
}

/// An unnotified row is retried, because the engine that owes the message may
/// not be running yet when the update finishes.
#[tokio::test]
async fn a_requester_that_cannot_be_reached_yet_is_told_later() {
    let f = fixture(Mode::Prompt);
    f.updater.check().await;
    f.updater.request(agent());
    let rt = FakeRuntime::never_quiet();
    rt.deaf.store(true, Ordering::SeqCst);
    f.updater.tick(rt.as_ref()).await;
    assert!(rt.notices.lock().unwrap().is_empty());
    assert!(
        !state::peek(&f.dir.join("update"))
            .history
            .last()
            .unwrap()
            .notified
    );

    rt.deaf.store(false, Ordering::SeqCst);
    f.updater.tick(rt.as_ref()).await;
    assert!(
        !rt.notices.lock().unwrap().is_empty(),
        "the notice is delivered once the engine is back"
    );
    assert!(state::peek(&f.dir.join("update"))
        .history
        .iter()
        .any(|r| r.notified));
}

#[test]
fn a_running_build_that_was_never_stamped_falls_back_to_the_checkout() {
    let driver = FakeDriver::new();
    assert_eq!(running_sha(TARGET, driver.as_ref()).unwrap(), TARGET);
    assert_eq!(running_sha("unknown", driver.as_ref()).unwrap(), RUNNING);
    assert_eq!(running_sha("", driver.as_ref()).unwrap(), RUNNING);
}

#[test]
fn every_outcome_has_its_own_message_and_empty_components_read_as_nothing() {
    fn row(outcome: Outcome, components: Vec<Component>, busy: Vec<String>) -> Row {
        Row {
            at: 0,
            from: RUNNING.into(),
            to: TARGET.into(),
            by: agent(),
            outcome,
            components,
            busy,
            notified: false,
        }
    }

    let updated = message(&row(Outcome::Updated, vec![Component::Engine], vec![]));
    assert!(
        updated.contains("updated") && updated.contains("(engine)"),
        "{updated}"
    );

    let no_components = message(&row(Outcome::Updated, vec![], vec![]));
    assert!(!no_components.contains("()"), "{no_components}");

    let rolled_back = message(&row(Outcome::RolledBack, vec![], vec![]));
    assert!(rolled_back.contains("rolled back"), "{rolled_back}");

    let timed_out_named = message(&row(
        Outcome::DrainTimedOut,
        vec![],
        vec!["pm".into(), "sdk".into()],
    ));
    assert!(timed_out_named.contains("pm, sdk"), "{timed_out_named}");

    let timed_out_unnamed = message(&row(Outcome::DrainTimedOut, vec![], vec![]));
    assert!(
        timed_out_unnamed.contains("agents were"),
        "{timed_out_unnamed}"
    );

    let refused = message(&row(
        Outcome::Refused(BlockReason::CiPending),
        vec![],
        vec![],
    ));
    assert!(refused.contains("waiting for CI"), "{refused}");

    let build_failed = message(&row(Outcome::BuildFailed, vec![], vec![]));
    assert!(build_failed.contains("did not build"), "{build_failed}");

    let smoke_failed = message(&row(Outcome::SmokeFailed, vec![], vec![]));
    assert!(smoke_failed.contains("smoke test"), "{smoke_failed}");

    let swap_failed = message(&row(Outcome::SwapFailed, vec![], vec![]));
    assert!(
        swap_failed.contains("could not be installed"),
        "{swap_failed}"
    );

    let interrupted = message(&row(Outcome::Interrupted, vec![], vec![]));
    assert!(interrupted.contains("interrupted"), "{interrupted}");
}

/// A fake of the OTHER side of the seam from `FakeRuntime`: one engine, as
/// `Registry` (not `Updater`) sees it. `Registry` is the piece that fans a
/// single drain out across every engine this daemon runs, and nothing above
/// exercises it directly — the `Updater` tests all go through `FakeRuntime`.
#[derive(Default)]
struct FakeEngine {
    log: Mutex<Vec<String>>,
    posts: Mutex<Vec<(Uuid, String)>>,
    busy_answer: Mutex<Vec<String>>,
}

impl FakeEngine {
    fn new() -> Arc<Self> {
        Arc::new(FakeEngine::default())
    }

    fn did(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }
}

impl EngineControl for FakeEngine {
    fn pause(&self) {
        self.log.lock().unwrap().push("pause".into());
    }

    fn busy(&self) -> wheel_engine::update::BoxFuture<'_, Vec<String>> {
        Box::pin(async move {
            self.log.lock().unwrap().push("busy".into());
            self.busy_answer.lock().unwrap().clone()
        })
    }

    fn resume(&self) -> wheel_engine::update::BoxFuture<'_, ()> {
        Box::pin(async move {
            self.log.lock().unwrap().push("resume".into());
        })
    }

    fn post_system(&self, agent: Uuid, body: String) -> wheel_engine::update::BoxFuture<'_, bool> {
        Box::pin(async move {
            self.posts.lock().unwrap().push((agent, body));
            true
        })
    }
}

/// `Arc::downgrade`'s type parameter is inferred from the `Arc<FakeEngine>` it
/// is given, so the unsizing to `Weak<dyn EngineControl>` `attach` wants has to
/// happen at a coercion site — a bare `Arc::downgrade(&a)` at the call site
/// infers `Weak<FakeEngine>` and stops there.
fn weak(e: &Arc<FakeEngine>) -> Weak<dyn EngineControl> {
    let w: Weak<FakeEngine> = Arc::downgrade(e);
    w
}

#[tokio::test]
async fn an_engine_that_joins_mid_drain_is_paused_at_once() {
    let reg = Registry::default();
    let a = FakeEngine::new();
    reg.attach(Uuid::nil(), weak(&a));
    assert!(a.did().is_empty(), "not paused before any drain starts");

    reg.pause().await;
    let b = FakeEngine::new();
    reg.attach(Uuid::new_v4(), weak(&b));
    assert_eq!(
        b.did(),
        vec!["pause".to_string()],
        "joins a drain already in progress"
    );

    reg.resume().await;
    assert!(a.did().contains(&"resume".to_string()));
    assert!(b.did().contains(&"resume".to_string()));
}

#[tokio::test]
async fn a_dropped_engine_falls_silently_out_of_busy() {
    let reg = Registry::default();
    let a = FakeEngine::new();
    let b = FakeEngine::new();
    *a.busy_answer.lock().unwrap() = vec!["agent-a".into()];
    *b.busy_answer.lock().unwrap() = vec!["agent-b".into()];
    reg.attach(Uuid::new_v4(), weak(&a));
    reg.attach(Uuid::new_v4(), weak(&b));

    let mut busy = reg.busy().await;
    busy.sort();
    assert_eq!(busy, vec!["agent-a".to_string(), "agent-b".to_string()]);

    drop(a);
    assert_eq!(
        reg.busy().await,
        vec!["agent-b".to_string()],
        "the dropped engine is gone, not merely quiet"
    );
}

#[tokio::test]
async fn notify_reaches_only_the_named_projects_engine() {
    let reg = Registry::default();
    let engine = FakeEngine::new();
    let project = Uuid::new_v4();
    let node = Uuid::new_v4();
    reg.attach(project, weak(&engine));

    let reached = reg
        .notify(
            &Requester::Agent {
                project,
                node,
                name: "pm".into(),
            },
            "hello".into(),
        )
        .await;
    assert!(reached);
    assert_eq!(
        engine.posts.lock().unwrap().clone(),
        vec![(node, "hello".to_string())]
    );

    let unreached = reg
        .notify(
            &Requester::Agent {
                project: Uuid::new_v4(),
                node,
                name: "pm".into(),
            },
            "hello".into(),
        )
        .await;
    assert!(!unreached, "no engine is attached for that project");

    assert!(
        reg.notify(&Requester::Operator, "logged, not delivered".into())
            .await
    );
    assert!(
        reg.notify(&Requester::Auto, "logged, not delivered".into())
            .await
    );
}

#[tokio::test]
async fn the_hook_delegates_to_the_updater_and_the_registry_it_was_built_from() {
    let f = fixture(Mode::Prompt);
    f.updater.check().await;
    let registry = Arc::new(Registry::default());
    let hook = Hook {
        updater: f.updater.clone(),
        registry: registry.clone(),
    };

    let notice = hook.notice().expect("a notice");
    assert_eq!(notice.target.as_str(), TARGET);

    let outcome = hook.request(wheel_engine::update::Requester {
        project: Uuid::nil(),
        node: Uuid::nil(),
        name: "pm".into(),
    });
    assert!(
        matches!(outcome, RequestOutcome::Accepted(_)),
        "{outcome:?}"
    );

    let engine = FakeEngine::new();
    hook.attach(Uuid::new_v4(), weak(&engine));
    let _ = registry.busy().await;
    assert!(
        engine.did().contains(&"busy".to_string()),
        "attach() reached the registry the hook was built with"
    );
}

/// `WHEEL_AUTO_UPDATE` is process-global and cargo runs tests in parallel
/// threads, so every case that reads or writes it is sequenced inside this
/// one test rather than split across separate `#[test]`s — same reasoning as
/// `wheeld::config`'s `configuration_comes_from_flags_then_environment_then_defaults`.
#[test]
fn from_operator_reports_status_and_records_a_request_exactly_when_asked() {
    let data_dir = std::env::temp_dir().join(format!("wheeld-op-{}", Uuid::new_v4()));
    let update_dir = data_dir.join("update");

    std::env::remove_var(policy::ENV_MODE);
    let mut out = Vec::new();
    let err = from_operator(&data_dir, false, &mut out).unwrap_err();
    assert!(err.to_string().contains("off"), "{err}");

    std::env::set_var(policy::ENV_MODE, "prompt");

    out.clear();
    from_operator(&data_dir, true, &mut out).unwrap();
    let text = String::from_utf8(out.clone()).unwrap();
    assert!(text.contains("has not checked yet"), "{text}");

    let store = StateStore::open(&update_dir).unwrap();
    store.update(|s| s.checked_at = Some(1)).unwrap();
    out.clear();
    from_operator(&data_dir, true, &mut out).unwrap();
    let text = String::from_utf8(out.clone()).unwrap();
    assert!(text.contains("nothing pertinent to update"), "{text}");

    let notice = UpdateNotice {
        state: UpdateState::Available,
        running: Sha::parse(RUNNING).unwrap(),
        target: Sha::parse(TARGET).unwrap(),
        components: vec![Component::Engine],
        commits: 3,
        reason: None,
    };
    store
        .update(|s| {
            s.last_notice = Some(notice.clone());
            s.record(Row {
                at: 1,
                from: RUNNING.into(),
                to: TARGET.into(),
                by: agent(),
                outcome: Outcome::SwapFailed,
                components: vec![Component::Engine],
                busy: vec![],
                notified: true,
            });
        })
        .unwrap();

    out.clear();
    from_operator(&data_dir, true, &mut out).unwrap();
    let text = String::from_utf8(out.clone()).unwrap();
    assert!(text.contains(&notice.line()), "{text}");
    assert!(text.contains("last attempt"), "{text}");
    assert!(
        !state::request_path(&update_dir).exists(),
        "--status must never record a request"
    );

    out.clear();
    from_operator(&data_dir, false, &mut out).unwrap();
    let text = String::from_utf8(out.clone()).unwrap();
    assert!(text.contains("requested"), "{text}");
    assert!(
        state::request_path(&update_dir).exists(),
        "the operator's own request is recorded"
    );

    std::env::remove_var(policy::ENV_MODE);
    std::fs::remove_dir_all(&data_dir).ok();
}
