# 051 — Idle-parking (§3c#14) adversarial review: SOUND on all five vectors, three low tightenings

- **Severity:** Low overall (the compute fix is sound; one Low-Medium tighten). Owner: SDK/Engine. Boundary TB4
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

## Tighten #1 (Low-Medium) — swallowed kill failure claims a release that may not have happened
`park` (mod.rs:829): `let _ = r.child.kill().await;` — the kill Result is discarded, then the token is revoked
and status set Parked UNCONDITIONALLY. If kill errored, park claims Parked (a saving) while the process may
still be alive (the ~162MB not released) — the exact "park that doesn't release" vector (2). It contradicts the
module's OWN principle: the no-child `else` branch says "marking it Parked would claim a saving that was never
made" — but that principle is not applied to the kill-FAILURE case. `kill_on_drop(true)` on the taken `Running`
is a real backstop (it drops at park's end → kill retried by the tokio driver), so the practical risk is low.
Fix: check the kill result; on failure, log and do not claim Parked (leave a state that reflects a still-running
process), or verify the release before revoking/parking. Worth doing before a restart-all-host deploy on a
compute-critical fix; otherwise it is backstopped and a fast-follow.

## Tighten #2 (Low) — `arm_park_timer` stacks a timer per turn, no cancellation
`arm_park_timer` (mod.rs:848) spawns a NEW `tokio::spawn(sleep→park)` each time it is armed (after every turn,
call site :1163). Nothing cancels a prior timer, so a chatty agent accumulates one sleeping task per turn for
the timeout window. They are HARMLESS (a stale timer's `park` re-check finds the agent busy/queued and no-ops,
or parks it if it is genuinely idle — correct either way), so this is a task-resource leak, not a correctness
bug. Cleaner: a single re-armable timer, or a generation token so only the latest arming can park.

## Tighten #3 (Low) — resume trusts the session_id blindly
Nothing validates the kept `session_id` before `--resume`. If the harness session is stale/expired, `--resume`
may error or silently start a fresh session (context loss). Preserving+passing the id is the engine's correct
job (done); confirm the FAILED-resume path falls back to a fresh session rather than wedging in `starting`
(041's class). Harness/resume semantics — cross-ref 041.

## Note
The three tightens are all Low(-Medium); none is a lost-message, silent-context-loss, or escalation. The
slot-lock serialization + under-lock re-check is the correct spine, and the enqueue→deliver invariant (now
load-bearing because parking makes "agent already running" false) is held across every production path today.
Recommend a test that pins "any enqueue path resumes a parked target," so a future enqueue-without-deliver
(the ingress-P0 shape) cannot strand a message behind a parked agent.
