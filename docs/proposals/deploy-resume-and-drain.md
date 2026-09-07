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
