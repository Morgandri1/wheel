# 032 — Host boot availability: poison-project grid-DoS is DEFEATED by design; the residual is the accepted SPOF sharpened by restartPolicyMaxRetries=10

- **Severity:** Low-Medium (availability). The tenant-triggerable path (a poison project crash-looping the
  shared host) is CLOSED by design — this finding is mostly a **regression guard** + two actionable residuals
  on the single-host SPOF. Owner: SDK/Engine (host reconcile) + API/infra (Railway restart policy). Boundary:
  multi-tenant host (all projects share one `wheel-host` process).
- **Status:** Reviewed in source @ HEAD. No live run (single-host availability is an architecture property).

## The concern (PM): a bad boot takes the whole grid down, no partial service, one host process for all
`infra/railway/settings.json`: host `restartPolicyType: ON_FAILURE`, `restartPolicyMaxRetries: 10`,
`healthcheckTimeout: 300`. If a host boot fails repeatedly, Railway restarts ≤10× then gives up → the whole
grid (every project) is down with no partial service. The sharp version: can ONE tenant force this?

## Poison-project → grid crash-loop: DEFEATED (verified in source — keep it this way)
A tenant CANNOT crash-loop the host. Three properties, all present:
1. **Reconcile is log-and-continue per project.** `lib.rs:150-153`:
   `if let Err(e) = state.sandbox.start(...) { tracing::error!(...) }` — comment: "One project failing to
   come back must not stop the others." A DB-read failure `skip`s reconcile (`:120-125`), does not crash.
2. **Per-project start is BOUNDED.** `process.rs:394` `deadline = now + start_timeout_secs` (default 30s,
   `config.rs:130`), loop probes `/healthz` then fails at the deadline. So a *hanging* engine (never
   healthz-green, never errors) times out → `Err` → caught by (1). A hang cannot block reconcile forever.
3. **Liveness is immediate.** `lib.rs:48-49,401`: `/healthz` returns 200 at once; project routes are 503 until
   reconcile finishes. So Railway's 300s healthcheck passes on liveness — a slow or failing reconcile does NOT
   trip the restart policy → no whole-grid crash-loop from reconcile.
So the worst a poison project does is add ≤30s to boot (its start times out) while its own routes are 503; the
host stays live and every other project reconciles. **This is a good design — credit to the host author.**

### REGRESSION GUARD (the reason this has a number)
If any of the three regresses, a tenant regains a whole-grid DoS: (a) reconcile changing from log-and-continue
to `?`/abort on a project error; (b) `sandbox.start` losing its timeout (a hang would then block reconcile at
that project → later projects never served, routes 503 forever); (c) reconcile moving in FRONT of liveness
(so a slow reconcile fails the 300s healthcheck → Railway crash-loops the host). A test should pin all three:
a project whose start errors AND one whose start hangs must both leave the host live and the others reconciled.

## Residual 1 (actionable) — restartPolicyMaxRetries=10 turns a recurring host-boot bug into a PERMANENT outage
The defeated path is tenant-triggered; a HOST-LEVEL boot failure (config parse, DB connect, socket bind, or a
panic in `serve`/reconcile — not tenant-reachable) still crash-loops the host process. With
`restartPolicyMaxRetries: 10`, Railway restarts 10× then STOPS → the entire grid is down permanently with no
auto-recovery until a human intervenes. Recommend: (a) remove/raise the retry cap (unlimited `ON_FAILURE`, or a
backoff) so Railway keeps retrying a transient bad boot rather than parking the grid dead; (b) external
alerting on host-down — the 10-retries-then-give-up MUST page someone, or the first grid-wide outage is
silent until a user reports it. The host is a documented SPOF (contract §5b), but "permanent after 10" +
"no alert" is the part that turns an incident into a prolonged one.

## Residual 2 (Low) — serial reconcile: N slow projects → 30N-second delayed grid return
Reconcile is a serial `for` loop (`lib.rs:131`), each start bounded at 30s. A batch of slow/hanging projects
adds up: 20 hanging projects → ~10 minutes before the grid's routes open (503 throughout, liveness fine). Not
a crash, but a slow full-grid return after any host restart. Recommend a bounded-concurrency reconcile or a
global reconcile budget so one batch of slow tenants doesn't push the whole grid's recovery into minutes.

## Not a finding / accepted
Single host = SPOF is contract-accepted for v1 (§5b). No partial service during a host restart is inherent to
one process for all tenants — worth stating the blast radius (all tenants) but it is the accepted architecture.
The verdict: the dangerous version (tenant-triggered grid crash-loop) is closed; keep it closed (regression
guard), lift the MaxRetries-10 permanent-outage trap, and alert on host-down.
