# Deploy resume / drain — current state, and the at-least-once ruling (PM + SDK, 2026-09-07)

Operator asked: can we GUARANTEE in-flight agents resume in place across a wheel-host restart?

## Honest answer: not today, and not "in place" ever cheaply
- Subprocess harnesses (Claude Code / Codex) do NOT checkpoint mid-turn, so a turn killed mid-execution
  cannot resume from its exact instruction — the realistic target is "no turn is killed mid-flight, and
  anything that was is re-run, and the session resumes," NOT "freeze and continue."
- TODAY the host does NOT handle SIGTERM at all (SDK). A Railway container swap SIGKILLs mid-turn agents.
  Sessions resume (session_id persisted, session store survives — observed: all six reconciled to parked
  with sessions intact on the 08:09 deploy; SDK to confirm the store path is on the persistent /data
  volume, Q4). But in-flight turns are LOST today.

## What it takes to make a deploy safe anytime
1. Graceful drain: engine catches SIGTERM, stops new turns, lets in-flight turns finish, persists, exits
   before SIGKILL. Bounded by Railway's grace window (Q5: SDK will not quote an unsourced number; it is
   moot until drain exists; measure it as part of the drain work).
2. Re-deliver what did not finish, on reconcile — the "sweep."

## RULING (PM, 2026-09-07): the redelivery sweep does NOT land without effectively-once
SDK asked for a dedup layer OR an explicit at-least-once+replay-tolerance ruling. Ruling: at-least-once
with BLIND replay is NOT acceptable on this board. The agents do real dev work with non-idempotent side
effects — a re-run turn can double-commit, re-push, or re-apply an edit. So:
- The sweep must land on a DEDUP / processed-marker layer (a message/turn id marked consumed before the
  side effect, checked on redelivery) so redelivery is effectively-once, NOT blind replay.
- Until that layer exists, the sweep does NOT ship. "Do not let anyone land that sweep on its own" — held.
- This is part of the drain work, not a standalone item, and it is wake-first (not now).

## Separate accuracy correction (SDK): Codex is M2, not working today
CODEX_HOME appears only in comments; there is exactly one harness driver, Claude, hardcoded in
Supervisor::new. If the pitch or any doc implies Codex agents work today, that is inaccurate — Codex is M2.
Flag wherever "claude code or codex" is stated as present tense.

## Sweep-ruling SCOPE clarification (SDK asked, PM ruled 2026-09-07) — BUG-036 is two bugs
SDK correctly checked the scope of the ruling above rather than assuming it. BUG-036 separates:
- (a) REDELIVERY of a stuck `delivered` message — BLOCKED by the effectively-once ruling above (blind
  re-run = silent double-apply). Held; part of drain work.
- (b) THE HEALTH SIGNAL LYING about it — NOT blocked. Pure observability: the engine already knows whether
  it holds a live process, so a `delivered` row with no live process is a wedge, not a live turn. Reporting
  it changes NO delivery behaviour, re-runs nothing, cannot double-apply. It is the only one of the six
  silent-failure defects where /healthz denies a state the system can never leave on its own — a liveness
  lie, not a quiet failure.

RULING: prepare (b) now on SDK's branch, gated and held like the codex guard; it lands after the wake with
the guard (engine code, redeploys the host). (b) does NOT land before the wake — the wake is watched by
explicit signals plus direct DB observation, so a delivered-wedge shows as signal 1 failing + a `delivered`
row with no live process (PM checks that directly during the wake); landing (b) pre-wake would cost a
deploy and re-open the gate for marginal benefit. The sweep ruling blocks (a) only; it never blocked (b).
