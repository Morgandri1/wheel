# 051 — Idle-parking (§3c#14) adversarial review: SOUND on all five vectors; one measured bug (QA BUG-040, premature park) + two low tightenings

- **Severity:** Low overall for the vectors (the compute fix is sound); ONE tightening turned out to be a real
  correctness bug on measurement — QA BUG-040, premature park (see Tighten #2, corrected). Owner: SDK/Engine. Boundary TB4
  (supervisor lifecycle). Reviewed at minutes-priority against the deploy target: **origin/sdk/query-function-denylist
  @ 47486f8** (park `supervisor/mod.rs:809`, `arm_park_timer:848`, call site `:1163`), read directly from the
  branch. High-stakes because it deploys to a restart-all host + broaden-wake.
- **Status:** Source review of the branch. Verdict: deployable; one item worth fixing pre-deploy.

## The load-bearing correctness property (vectors 3 + 5)
`park()` takes the SLOT LOCK and holds it across BOTH the re-check and the kill; `deliver()`/`pump_queue` take
the SAME lock. So park and delivery SERIALIZE — they cannot interleave. Under the lock, park re-checks
`status == Idle` AND `!has_queued`; either false → return without parking. Therefore:
- **(3) park mid-task:** impossible — Running (mid-turn) or queued-work both make the re-check return; and while
  park holds the slot lock no delivery can start.
- **(5) timer-vs-message race:** whichever takes the slot lock first wins. Park-first → kill+Parked, then the
  message's `deliver` takes the lock, sees Parked+queued, resumes. Deliver-first → status Running, park's
  re-check returns. No lost or double-run. A message arriving during a park is durable in the queue and resumes
  via `deliver`; worst case is a park+immediate-resume churn (minor).
This is the right design.

## Per-vector
1. **session_id / context loss on resume:** park keeps `session_id` (it kills the child + revokes the token +
   sets Parked; it does NOT `clear_session`). `deliver → start` resumes with `--resume <session_id>`. Verified.
   (Edge below.)
2. **park that doesn't release the process:** `guard.take()` removes the Running from the slot and
   `r.child.kill().await` kills it; memory is freed. See the tighten — the kill Result is swallowed.
3. **park mid-task:** covered by the under-lock `Idle && !has_queued` re-check. Sound.
4. **resume-after-park wedging forever:** resume uses the standard `deliver → start` path. "Parked forever" is
   BY DESIGN for an UNMESSAGED agent (the point of parking); a message always triggers `deliver`. Verified all
   three production enqueue paths call `deliver` after `enqueue`: user-send (`/v1/agents/:id/send`), cli `msg`,
   and ingress — so a message cannot strand behind a freshly-parked agent. SDK also has a stalled-message
   detector that treats "parked with old work" as the resume path's job, not a stall. A resume that HANGS is
   041's class (start-not-advancing), not new here.
5. **timer/message race:** sound (above).

## Tighten #1 — FIXED & CONFIRMED (d44f8fd) — swallowed kill failure claims a release that may not have happened
`park` (mod.rs:829): `let _ = r.child.kill().await;` — the kill Result is discarded, then the token is revoked
and status set Parked UNCONDITIONALLY. If kill errored, park claims Parked (a saving) while the process may
still be alive (the ~162MB not released) — the exact "park that doesn't release" vector (2). It contradicts the
module's OWN principle: the no-child `else` branch says "marking it Parked would claim a saving that was never
made" — but that principle is not applied to the kill-FAILURE case. `kill_on_drop(true)` on the taken `Running`
is a real backstop (it drops at park's end → kill retried by the tokio driver), so the practical risk is low.
Fix: check the kill result; on failure, log and do not claim Parked (leave a state that reflects a still-running
process), or verify the release before revoking/parking. Worth doing before a restart-all-host deploy on a
compute-critical fix; otherwise it is backstopped and a fast-follow.

**CONFIRMED FIXED (d44f8fd, all three sites — park:835, stop:887, clear_context:1370):** each now
`if let Err(e) = r.child.kill().await { tracing::warn!(%agent, error=%e, ...) }` — the kill failure is LOGGED
(visible), no `let _ =` swallow remains, and kill_on_drop stays the backstop for the actual release. That is the
agreed one-liner (remove the SILENT part; rely on kill_on_drop for the kill). Verified against the branch @
d44f8fd, which builds on the reviewed 47486f8. This was the last merge-gate review item — GREEN from red-team.

## Tighten #2 — CORRECTED: `arm_park_timer` stacks a timer per turn → PREMATURE PARK (QA BUG-040), not harmless
`arm_park_timer` (mod.rs:848) spawns a NEW `tokio::spawn(sleep→park)` each time it is armed (after every turn,
call site :1163), with no cancellation. I first rated the stacking HARMLESS — "a stale timer's `park` re-check
finds the agent busy and no-ops, or parks it if genuinely idle, correct either way." **That was wrong, and QA
MEASURED it (BUG-040): a stale timer parks an agent BEFORE its idle_timeout has elapsed.** My error: the
re-check (`status==Idle && !has_queued`) verifies "idle NOW," not "idle LONG ENOUGH since last activity." So a
timer armed after turn 1 fires `idle_timeout` after turn 1 — but if the agent did turn 2 in between, that is
BEFORE the correct park time (idle_timeout after turn 2), and the agent, being idle-now, is parked early. My
reasoning held for the mid-task case (the slot lock, which is right) and missed the premature-park case
entirely. Measurement beat the reading — logged as calibration, and the reason this class of claim must be run,
not reasoned. Fix: QA's option 2 — on fire, check elapsed-since-last-activity and RE-ARM for the remainder
rather than park; a single re-arming timer also closes the task-leak I flagged. Severity is a real correctness
bug (an agent parked early = a needless resume + latency on the next message), not the "leak only" I first said.

**FIXED (cbc6b4a, on main):** park now compares `last_activity` and RETURNS the remaining seconds; the timer
loop re-waits that remainder, so a stale per-turn timer self-corrects instead of parking early. Closes the leak
too (one self-correcting wait rather than a task per turn). Verified in source.

## Tighten #3 (Low) — resume trusts the session_id blindly
Nothing validates the kept `session_id` before `--resume`. If the harness session is stale/expired, `--resume`
may error or silently start a fresh session (context loss). Preserving+passing the id is the engine's correct
job (done); confirm the FAILED-resume path falls back to a fresh session rather than wedging in `starting`
(041's class). Harness/resume semantics — cross-ref 041.

**REVIEWED (cbc6b4a): CLEAN, Low residual — no systematic false-positive session-wipe.** The clear fires only
in `reap()` (child EXITED) when a `--resume` start exited WITHOUT ever emitting init — a definitive
unusable-session signal, not a timeout, so a valid-but-slow init is never false-wiped. reap's run_id guard
(`if slot run_id != this run_id { return }`) means a resumed agent killed PRE-INIT by stop/park/clear does not
reach the clear (the killer took the slot). The sole residual: a VALID session whose child dies pre-init ON ITS
OWN from a TRANSIENT cause (OOM/crash) gets its context cleared -> fresh next start (recoverable, not a wedge);
SDK documents this as a deliberate prefer-fresh-over-wedged trade. Does not block broadening the wake.
Source-verified (trigger + run_id guard read in cbc6b4a); the one live-check to make it run-verified is a
resumed agent killed pre-init keeping its session vs one exiting pre-init on its own clearing it — offered.

## Note
Correction: Tighten #2 is NOT low — QA measured it as BUG-040 (premature park); I had reasoned it harmless.
Tighten #1 (swallowed kill) is Low-Medium and #3 (stale resume) is Low; none of the three is a lost-message,
silent-context-loss, or escalation. The
slot-lock serialization + under-lock re-check is the correct spine, and the enqueue→deliver invariant (now
load-bearing because parking makes "agent already running" false) is held across every production path today.
Recommend a test that pins "any enqueue path resumes a parked target," so a future enqueue-without-deliver
(the ingress-P0 shape) cannot strand a message behind a parked agent.
