# 041 — `starting` has no bounded deadline: a spawned-but-silent child abandons the agent there forever (class, not the one call site)

- **Severity:** Medium (liveness/availability; becomes High for the host the day the per-agent run cap lands).
  Owner: SDK/Engine. Boundary TB4 (engine ↔ child). Written to answer PM's question — "is `starting` a
  state an agent should be ABANDONED in at all, or is the real defect the absence of a timeout out of it?" —
  at the CLASS level, because SDK is fixing the one call site that hung and we must not fix only the instance.
- **Status:** CONFIRMED by source trace (all line refs below at origin/main a3c9656). **See the CORRECTION at
  the bottom: PM caught, live, that the deadline must key on an in-flight/delivered turn (`running`), NOT on
  time-in-`starting` — a `starting` wall-clock would kill healthy agents idle on an empty stdin. Read the
  correction before acting; the trigger stated in the middle of this finding is superseded by it.**

## The answer to the question, stated first
`starting` is legitimately a state an agent CAN be abandoned in, and you cannot design that away — so the real
defect is the ABSENCE OF A BOUNDED DEADLINE out of it. Here is why the "should it exist / be abandonable"
framing is the wrong one:

Every other live state has an engine-side driver or is a rest state. `running → idle` is driven by the
harness `Result` event, with `interrupt` as the escape if a turn hangs. `idle` and `parked` are rest states.
`stopped`/`error`/`budget_exhausted` are settled. `needs_auth` waits on the operator but is settled and
visible (and NOT `is_live`, so it holds nothing). **`starting` is the unique transient whose only legitimate
exit is an event from a process the engine spawned but does not control the internals of.** The intended exit
is `HarnessEvent::Init` (supervisor/mod.rs:770 → `Idle`); the only other exits are a death/error event
(mod.rs:868/1007) or explicit `stop()`. A child can `spawn()` cleanly (mod.rs:616 — no error to catch) and
then block forever BEFORE it prints its init line — hung on the MCP stdio handshake, a model/network call
during init, a git operation, or simply a `claude`/`codex` that started and stalled. In that case
`lines.next_line().await` (mod.rs:761) blocks on a line that never comes: no `Init`, no death, no error. The
child's SILENCE is the abandonment, and no redesign of the state removes the child's ability to be silent.

So the fix is not "forbid/remove `starting`" — it is "bound the wait." A silent child is turned from an
invisible unbounded hang into a bounded, visible, recoverable failure by a supervisor-owned deadline.

## The clincher that it's the timeout — the pattern is already blessed one layer up
The host↔engine spawn contract ALREADY imposes exactly this discipline on ITS children: the engine "must be
healthy (`GET /healthz`) within 10s" and host `start` "blocks until the engine is green (≤30s) or returns 504"
(§4b). The platform demands a readiness deadline of a sandbox; the engine grants NONE to its own children
(agents). `starting` for an agent is the same transient as sandbox-start, missing the same deadline. Adding it
is consistency with a rule we already trust, not a new mechanism.

## Verified facts
1. **No deadline exists.** The only `std::time::Instant` in supervisor/mod.rs is a test helper (mod.rs:1392,
   under `#[cfg(test)]`). `start()` sets `Starting` (mod.rs:528), spawns (616), wires the stdout/stderr pumps
   (635-637), returns `Ok(Starting)` (639) — and nothing anywhere bounds the wall time in `Starting`.
2. **`Starting` counts as `is_live`** (state.rs `is_live` = `Starting | Running | Idle`). So the engine treats
   an abandoned-in-starting agent as alive and able to take stdin.
3. **The paired queue-stall (current, real).** `pump_queue` (mod.rs:664) gates ONLY on a live slot existing
   (`guard.as_mut()`, and the slot is `Some` from mod.rs:622, before spawn) and `in_flight.is_none()` — it does
   NOT require `status == Idle`. So a message is delivered to a pre-`Init` hung child, which (a) flips status
   to `Running` at mod.rs:726, MASKING the hang as a healthy-looking "running", and (b) sets `in_flight`, which
   the never-arriving `Result` never clears → the queue is stalled forever and the message is stuck
   `Delivered`-never-`Consumed`. This is the §3c#15 failure ("liveness wrong / messages not processed") in a
   new guise: the supervisor knows "pid alive" but not "made progress", and a hung-but-alive child defeats it.
4. **The future cap-leak.** §3c/#14 promises a "per-host cap on concurrently RUNNING agents (default 32) with a
   fair queue." It is NOT implemented in the engine today (the only semaphore is host-level, wheel-host
   lib.rs:176, for sandbox ops). When that cap lands it will almost certainly gate on `is_live` — so every
   agent abandoned in `starting` will hold a permit it never releases, and N of them permanently lower the
   host's start capacity: a slow, invisible availability DoS. An agent that can influence its own start (a
   workspace git clone that hangs, an `mcp.url` that never handshakes) could induce it deliberately.

## Impact
Now: the agent hangs invisibly (UI shows `starting`, or `running` after item 3 — both look benign, no
`last_error`, no alarm); its queue stalls; its node token stays live (only `stop()` revokes it, mod.rs:653).
When the run cap lands: the above plus a per-host permit leak.

## Fix (class, not instance) — for SDK
1. **A supervisor-owned wall-time deadline on `Starting`**, set when it is entered, measured by the engine, NOT
   by any signal from the child (a hung child emits nothing; anything that waits for the child to report its
   own slowness is the same bug). Wrap the "await `Init`" in `tokio::time::timeout` / `select!` against a
   configurable `startup_timeout_secs`. Default generously (e.g. 60–120s) and, because a cold workspace clone
   or first `cargo`/`pnpm` fetch can legitimately be slow (see the toolchain caches at mod.rs:581), either give
   the clone/materialise step its own budget or make the timeout config per agent.
2. **On expiry: the supervisor kills the child and settles VISIBLY.** `kill_on_drop` is set, but the watchdog
   must actually drop/kill the slot, revoke the token as `stop()` does (mod.rs:653), and `set_status(Error,
   Some("did not reach Init within Ns"))` — or a distinct `startup_timeout` reason — so it is visible AND the
   slot/token/(future permit) are released.
3. **`parked → starting → running` resume (§3c#14) shares the SAME deadline.** A parked agent woken by a
   message hangs on resume identically; a watchdog that covers only cold start is a half-fix — exactly the
   "fixed the instance, not the class" outcome. `run_on_startup` agents start parked (compute-frugality (c)),
   so the deadline applies whenever a live child is actually attempted, not at board load.
4. **Close the queue-stall (item 3) at the same time.** Deliver only from `idle` (post-`Init`), never from
   `starting` — which also matches §3c#13 ("a message never starts a process; the single session consumes it
   when idle"). Alternatively the watchdog must clear a stuck `in_flight` on kill. Delivering-from-idle-only is
   the cleaner rule and removes the status-masking in item 3.

## CORRECTION (PM caught this live — the original trigger was wrong and would kill healthy agents)
PM observed a real, HEALTHY agent sitting in `starting`: "correctly idle with an empty stdin, waiting for a
message nobody sent," and noted a 60s deadline on `starting` would have killed it. PM is right, and it exposes
an error in the fix I proposed above. Tracing it with the harness in hand:

- Claude runs `--print --input-format stream-json` (headless streaming): it emits `system/init`/`result` around
  PROCESSING A TURN, and the ONLY `Starting → Idle` transition is `HarnessEvent::Init` (mod.rs:770). So a child
  with nothing written to its stdin never emits `Init` and legitimately STAYS in `starting`.
- `run_on_startup` agents come up **Parked**, and `deliver` (mod.rs:1120) calls `start()` only when a message is
  queued; `pump_queue` then writes the turn and sets **Running** (mod.rs:726) immediately. So a *delivered* turn
  leaves `starting` at once — which means an agent LINGERING in `starting` almost always has an EMPTY queue and
  nothing in-flight. That is healthy-waiting (or a startup-wedge, and the two are indistinguishable from
  outside, because the harness emits nothing until it is given a turn).
- Therefore the genuine hang this finding is about — a turn delivered, no result — actually lives in **`running`
  with `in_flight` set**, NOT in `starting`. I attached the deadline to the wrong state. A wall-clock on
  "entering `starting`" measures the wrong thing and converts a DELIVERY bug (a message that should have been
  enqueued but was not — the real defect in PM's incident, a §3c#15 dropped-message) into an agent-killing bug:
  it would kill the healthy victim and mask the upstream bug.

**Corrected fix — the deadline keys on UNRESPONSIVENESS-TO-WORK, never on time-in-state:**
1. Arm a deadline only while the child OWES a response: a turn has been written (`in_flight` is `Some`, i.e.
   status `running`) OR work is queued and `pump_queue`'s `write_all` is not making progress. No queued work and
   nothing in-flight ⇒ NO deadline. This is PM's empty-queue carve-out, stated as the precise trigger.
2. On expiry of an in-flight turn with no `result`: interrupt/kill and settle VISIBLY (`error`,
   "no result within Ns"), revoke token, release the slot — as before, but scoped to a delivered turn.
3. `starting` itself needs no wall-clock. A started, unmessaged agent waiting on empty stdin is healthy and must
   be left alone (or, cosmetically, surfaced as `idle`/`waiting` rather than a bare `starting` that reads as
   "coming up" — but that is presentation, not a kill condition).

The queue-stall in item 3 above stands and is the same defect seen from the other side: a turn delivered to a
child that never produces a `result` sets `in_flight` forever with no bound. The fix is the in-flight deadline
(corrected trigger), not a `starting` timer.

## CONFIRMED ROOT CAUSE (P0, already fixed by SDK) — validates the correction
PM confirmed the incident's actual bug: **ingress called `start()` (which spawns but never pumps) instead of
`deliver()`** (start + pump_queue). So the message WAS enqueued by ingress, but because `pump_queue` was never
invoked, the queued turn was never written to the child's stdin — the agent spawned, sat in `starting` on an
empty stdin with an undelivered queued message, and looked idle-but-wedged. Exactly the shape this finding is
about, and exactly why a `starting` wall-clock would have been the wrong instrument: it would have killed the
healthy child and masked the delivery-path bug (§3c#15). The P0 fix — call `deliver()`, not `start()`, on the
ingress path — is the root fix; an in-flight/queued-not-progressing deadline (corrected trigger above) is the
backstop that would have made such a stall VISIBLE (a queued message not draining) instead of a silent wait.
Production is verified healthy end to end (public webhook → parked agent → reply over Telegram, nothing left
queued).

## MEASURED ROOT CAUSE of the "lives in `starting`" instance — ephemeral_context restart into an empty queue
PM measured the discriminator: the ONLY agent stuck in `starting` is the ONLY one with `ephemeral_context=true`
(the operator's pm agent); the other five (ephemeral=false) park normally on the same engine. Status goes to
`starting` ~2s AFTER a completed turn — a restart, not a stalled start. I traced the mechanism in source:

1. A turn's `result` sets **Idle** (mod.rs:868).
2. Because ephemeral, the post-result branch (mod.rs:883) calls `clear_context` (mod.rs:1048): it kills the
   child, `clear_session`, then **`start()` → sets `Starting` (mod.rs:528)** and spawns a FRESH child + a FRESH
   `pump_stdout`, then `pump_queue` (mod.rs:1061).
3. The turn's message was already consumed, so the queue is **empty** — nothing is written to the fresh child's
   stdin. Claude (`--print --input-format stream-json`) emits `system/init` only when it PROCESSES a turn, so a
   fresh session with empty stdin emits no init; the (correctly armed) new `pump_stdout` has no init to handle;
   the only Starting→Idle transition (`Init`, mod.rs:770) never fires. Status **lives in `starting`** until the
   next message arrives. Non-ephemeral agents take the `else` at mod.rs:887, so their Idle (868) stands.

**Answer to SDK's discriminating question ("never comes" vs "comes and we drop it"): NEVER COMES.** `start()`
does re-arm `pump_stdout` for the restarted child (verified — mod.rs:635-637 via clear_context→start), so it is
not a dropped event and not a missing reader. There is simply no `init` to read, because the ephemeral restart
creates a fresh session with nothing to process. (The one premise to confirm in 30s: that a fresh `claude
--print --input-format stream-json` emits no `system/init` before its first turn. The measured "stuck 4+ min in
starting" is consistent with it, and the code rules out a drop; either way the fix below is the same.)

**Two bugs, not one:**
- Display: a healthy ephemeral agent reads as `starting` (looks broken; cost PM an investigation; would be
  killed by a naive time-in-starting deadline every turn — the exact mistake this finding's correction avoids).
- **Efficiency (P1):** it never reaches `idle`, so idle-parking (§3c#14) — which keys on idle — never fires.
  The ephemeral agent holds a LIVE harness process 24/7 between turns. This is the operator's OWN most-used
  agent (the only ephemeral one), and it is precisely the "one live process forever" cost the project exists to
  avoid — sharpened by the 70%-of-seven-day rate-limit warning the harness is already emitting.

**Fix (SDK) — settle the restart, do NOT eliminate it.** Restarting into a fresh session is what
`ephemeral_context` is FOR, so the fix keeps the restart and only fixes where it LANDS. Make it CONDITIONAL on
the queue, because the bug is exactly the empty-queue case:
- **Queue non-empty after an ephemeral turn:** restart-and-drain AS NOW — `clear_context`→`start()`→`pump_queue`
  writes the next message, so it reaches `running` and processes in a fresh session. This path already works;
  do not touch it. (Unconditionally parking here would STRAND the already-queued messages, since only a new
  enqueue re-triggers `deliver()` — a new bug in place of the old one.)
- **Queue empty after an ephemeral turn:** do NOT spawn a fresh child that sits in `starting`. `clear_session` +
  settle to **`Parked`** (no live process), and let `deliver()`'s existing parked→resume path start a FRESH
  session (no `--resume`, session cleared) on the NEXT message. This settles the status honestly and restores
  idle-parking for ephemeral agents — fixing both the display bug and the never-parks cost bug.
`wheel ctx clear` on demand settles the same way (park if idle/empty; drain if work remains).

## Note
This correction and the measured root cause are the important content. Credit to PM for the live catch and the
ephemeral discriminator; my original time-in-`starting` trigger would have killed pm every turn, forever.
