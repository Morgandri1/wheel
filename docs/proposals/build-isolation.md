# Build isolation — the invariant, and the runtime-coupling gap (PM, 2026-09-07)

Surfaced by the operator's question: "is the cloud board doing development in the same environment it's
running in?" Answer, made durable so it is a property not a habit.

## INVARIANT (how self-development stays safe): the running engine is never self-modified in place
Agents on the board develop SOURCE, not the running binary:
- An agent's workspace is a git WORKTREE off a shared object store on `/data` — a separate source checkout,
  not the deployed engine binary. (Verified in the first wake: `repos/wheel-<hash>` store + a 7.1M worktree.)
- The dev loop is edit -> commit -> PUSH to origin. New code ships the normal way: push -> CI builds+tests
  on an isolated runner -> Railway rebuilds and redeploys a NEW wheel-host. The live engine is replaced by a
  deploy, never patched in place by an agent.
- **BUILD HAPPENS OFF-BOARD.** The clone-and-build capability is exercised in CI (the `wheel-on-wheel` job,
  isolated GitHub runner), not in the running container. An agent must not `cargo build` Wheel inside the
  wheel-host container as part of its dev loop — build+test+deploy is CI/Railway's job. This keeps a heavy
  build off the CPU/RAM/disk the live engine is using.

## THE GAP (co-located, not fully isolated) — tracked, not closed
Source-dev is isolated (separate checkout, push-not-in-place, build off-board). The RUNTIME is not:
- The workspace and any process an agent runs share the CONTAINER, `/data` volume, project uid, and
  CPU/RAM/disk with the running engine. One container/uid per PROJECT (process backend).
- **Resource coupling:** if an agent ever runs an in-container build (or any heavy job), it contends with the
  live engine unbounded. Mitigation owed: resource limits on agent-run processes, and enforce build-off-board.
- **uid coupling:** the shared project uid is F007 (SDK measured: every node's token readable by any co-located
  agent). Already tracked; per-node uid (§2 base+1+n / 037/038) is the fix, queued post-wake.

## Acceptance
- "Build off-board" is an invariant: CI builds, the container does not. If a dev flow needs an in-container
  build, it gets an explicit resource bound first.
- Resource-coupling filed here; uid-coupling is F007/037/038. Both are runtime-isolation hardening, post-wake.

## Workspace data is PERSISTENT, not ephemeral (operator requirement, verified 2026-09-07)
Requirement: project workspace data (git repos, worktrees, etc.) must not be ephemeral. VERIFIED met:
- `/data` is a real ext4 volume mount (`/dev/zd10400`), a Railway persistent volume — NOT the container's
  ephemeral overlay fs.
- All workspace data is on it: the shared git object store (`repos/wheel-<hash>`), the agent worktrees
  (`ws/<agent>`), the session store (`creds/<uuid>/session-env/<session-id>`). Survives every container swap
  (253 session transcripts persist across every deploy this session).
- So uncommitted edits in a worktree are files on `/data` and survive a redeploy.

DISTINCTION from the deploy-drain gate: a redeploy kills the in-flight PROCESS/turn, NOT the data. The agent
resumes its session (--resume, proven) and re-runs the killed turn; its worktree files are intact. No data
loss on redeploy; only turn-progress loss (which deploy-drain additionally prevents).

GUIDANCE: `/tmp` IS container-ephemeral (not on `/data`). Agents keep working data in their workspace
(`ws/`, on `/data`), never in `/tmp` expecting persistence.
