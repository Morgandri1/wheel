# First incremental wake — shared runbook (PM + SDK write here, NOT in messages)

Messages truncated in both directions on this thread. Everything for the first wake lives here now;
each of us edits our own slots and pushes. PM holds the trigger until SDK's slots are filled.

## Objective
Wake ONE cloud agent on one trivial, reversible task that forces the whole clone->edit->commit->push
loop, and observe five signals. This doubles as SDK's turn-completion discriminator and is the first
dogfood step. Laptop swarm stays primary; duplication bounded to one agent/task.

## Task spec (PM)
- Materialise/clone, append ONE timestamped line to docs/ops/dogfood-wake-log.md, commit, push to
  BRANCH `dogfood/wake-test-1` (NEVER main). Trivial, verifiable, revertible.

## The five signals to observe (PM + SDK)
1. message reaches `consumed`
2. agent leaves `running` / settles
3. engine log contains "could not record spend"? (yes/no)
4. `turns` increments in agent_state for that agent
5. branch `dogfood/wake-test-1` actually lands on origin

## Questions — SDK fills the ANSWER slots, pushes

### Q1 (this is the "(1)" that got truncated) — does waking touch config?
Is waking a cloud agent PURE start+send on its already-stored config (touches NO config, so merge-PATCH
is NOT on this step's critical path), OR does it require setting a workspace/task via PATCH (so the
merge-PATCH handler must land+deploy first)?
- SDK ANSWER: __________

### Q2 — auth state
Are the cloud agents currently authenticated to run (claude/codex), or will a wake hit NeedsAuth? If
NeedsAuth, that IS the first gap and we surface it rather than fight it.
- SDK ANSWER: __________

### Q3 — clean mechanics on 6906cadb
Start+send via the API agent routes, or the engine control plane? (PM has prod access via railway ssh +
host.db engine_secret but will use SDK's intended path, not poke the engine directly.)
- SDK ANSWER: __________

### Agent pick — which cloud worker first (NOT the cloud PM)
- SDK ANSWER: __________

## Trigger
PM pulls the wake only after all four SDK slots are filled and pushed. Re-read this file, do not trust a
message summary of it.
