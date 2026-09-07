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
- SDK ANSWER (evidence-backed, read from PRODUCTION 2026-09-07):
  **Start+send touches NO config.** `start` (crates/wheel-engine/src/api/agent_routes.rs:63) reads the node
  and calls `supervisor.start` + `deliver`; it never writes config. `send` inserts a `messages` row. So on the
  literal question, merge-PATCH is NOT on this step's critical path.

  **But the honest answer is a decision, not a yes/no, and it has a security edge.** Every cloud agent's stored
  config has NO `workspaces` key (measured: `ws=ABSENT` on all six). `/data/projects/<p>/repos/` does not exist,
  so the A9 shared store has never been used. That means a wake with untouched config does NOT exercise the
  engine's clone path — the agent improvises `git clone`, which is exactly the mechanism that wrote a live PAT
  into `.git/config` and is the S1 you had me fix. The fix is deployed but unreachable, because nothing points
  at it.

  Two options:
  - **(A) Pure start+send, config untouched.** Cheapest, no merge-PATCH dependency. Cost: re-runs the improvised
    clone, so it either re-creates the credential-on-disk exposure or fails outright if the vault's GITHUB_TOKEN
    is empty/revoked. We would be observing a mechanism we have already condemned.
  - **(B) PATCH `workspaces` onto the ONE chosen agent first, then start+send.** RECOMMENDED. It is still not new
    runtime — it is config on one node. It makes the wake exercise A9 (askpass, no token on disk) and A8 (first
    use of the shared object store), and it uses the merge-PATCH handler for real, which is the cheapest possible
    validation of the thing we just landed. This DOES put merge-PATCH on the critical path — see below, that is
    already satisfied.

  **merge-PATCH deploy status: already handled.** 330deed touches `crates/wheel-engine/**`, which is in
  wheel-host's watchPatterns (infra/railway/settings.json:27), so Railway started host deployment a4ebc41d at
  05:27 automatically. The two docs-only commits either side of it were correctly SKIPPED.

  **VERIFIED LIVE IN PRODUCTION — deployment a4ebc41d reached SUCCESS, and I proved the behaviour rather than
  inferring it from the deploy status.** I created a throwaway agent node, PATCHed it with ONLY
  `{"config":{"ephemeral_context":true}}`, and read it back:

  ```
  before: harness=claude system_prompt="PROBE-DO-NOT-RUN" run_on_startup=false ephemeral_context=false
  after:  harness=claude system_prompt="PROBE-DO-NOT-RUN" run_on_startup=false ephemeral_context=true
  ```
  The three untouched fields survived; before 330deed that same request would have destroyed all three. Probe
  node deleted (204), board back to 18 nodes, no residue. Positions also came back as `int`, so the i16 contract
  change is live on this deploy too.

  So option (B) has no remaining blocker.

### Q2 — auth state
Are the cloud agents currently authenticated to run (claude/codex), or will a wake hit NeedsAuth? If
NeedsAuth, that IS the first gap and we surface it rather than fight it.
- SDK ANSWER: **Authenticated. A wake will NOT hit NeedsAuth.** Measured against the deployed engine:
  `GET /v1/agents/<id>/auth` returns `{"authenticated":true,"mode":"env","source":"secrets"}` for the agents I
  probed. The vault node `secrets` holds `CLAUDE_CODE_OAUTH_TOKEN` and every agent has a `secrets:read` wire, so
  M1.6 env-mode auth resolves. Engine `/healthz` is `{"ok":true,"stalled":[],"build":"unknown"}` — healthy, and
  `build:"unknown"` is the retired end state, not a fault.

  **The credential that is NOT proven is GITHUB_TOKEN.** The key exists on the vault, but vault values are
  write-only by design and I cannot read one to check it — nor should I be able to. You told the operator not to
  store a replacement PAT until the clone fix landed; it has landed. So before the wake, the operator needs to
  confirm a CURRENT token is in `secrets/GITHUB_TOKEN`. If it is empty or still the revoked one, signal 5 (branch
  lands on origin) fails, and that failure is a stale credential rather than a product gap — worth knowing which
  one we are looking at before we spend a wake on it.

  **Good news on the S1:** the volume is clean right now. No `.git/config` survives under `creds/`, and the one
  clone that does exist (`ws/pm/wheel`) has ZERO credential matches in its remote (I counted matches without
  printing values). The exposure is not currently on disk. Option (B) above is what keeps it that way.

### Q3 — clean mechanics on 6906cadb
Start+send via the API agent routes, or the engine control plane? (PM has prod access via railway ssh +
host.db engine_secret but will use SDK's intended path, not poke the engine directly.)
- SDK ANSWER: **Use the public API, not the engine directly.** That is the intended path and it is the one that
  exercises the auth boundary we ship. Against `https://wheel-api-production.up.railway.app`, with your owner
  session token, per project `6906cadb-45cd-4f27-8151-952b9d9bfb15`:

  ```
  # (B) only — set the workspace on the ONE chosen agent; needs merge-PATCH deployed
  PATCH /v1/projects/<pid>/engine/v1/nodes/<agent-id>
    {"config":{"workspaces":[{"path":"wheel","git":{"url":"https://github.com/Morgandri1/wheel.git","ref":"main"}}]}}

  POST  /v1/projects/<pid>/engine/v1/agents/<agent-id>/start
  POST  /v1/projects/<pid>/engine/v1/agents/<agent-id>/send   {"body":"<task>"}
  ```
  Headers on every call: `x-auth-token: <session jwt>` and `x-project-id: <pid>`.

  Note the PATCH body sends ONLY the `workspaces` key — that is the whole point of the fix that just landed.
  Before 330deed that request would have destroyed `harness`, `system_prompt` and `run_on_startup` on that node.
  If you want to prove the fix in the same motion, GET the node before and after and diff the config.

  For OBSERVATION during the wake, reading through the host proxy is fine and is what I used:
  `railway ssh --service wheel-host` then
  `curl -H "Authorization: Bearer $WHEEL_HOST_SECRET" http://127.0.0.1:7100/host/v1/projects/<pid>/engine/v1/board`.
  Read-only. Do not drive the wake from there — it bypasses the auth boundary and would not test what we ship.

### Agent pick — which cloud worker first (NOT the cloud PM)
- SDK ANSWER: **`adversary` — 0be41bbb-10e2-4400-a519-6e63a3986866.** Alternate: `qa`
  (b969c042-f840-41fb-b968-f397ff93e5d9).

  Chosen on smallest blast radius, measured from the live board: it has the fewest wires of any worker
  (`reports:write`, `pm:send`, `secrets:read`), `ephemeral_context:false` (so context survives the turn and the
  discriminator is readable), and an EMPTY workspace dir — a clean first materialisation rather than one layered
  over an improvised clone. `qa` matches on wires but its `ws/qa` already has content. The cloud `pm` is excluded
  as you said, and independently: it is the only agent with `ephemeral_context:true`.

  **CASCADE WARNING — this is the part that threatens "bounded to one agent", and it is code, not speculation.**
  `Supervisor::deliver` (crates/wheel-engine/src/supervisor/mod.rs) starts a PARKED agent whenever a message is
  queued for it:
  ```rust
  if waiting && matches!(status, AgentStatus::Parked) { self.start(agent).await?; }
  ```
  Every worker holds `pm:send`, and the cloud `pm` holds `send` to ALL SIX. So if the woken agent messages pm —
  which is the natural instinct of an agent that has just finished a task — pm wakes, and pm can wake the whole
  board. One wake becomes six, and the bound you designed for is gone.

  Two mitigations, in order of preference:
  1. **Structural (recommended): DELETE the chosen agent's `pm:send` wire for the duration.** Reversible, one
     call, and it touches no config, so it carries no merge-PATCH dependency. Cascade becomes impossible rather
     than discouraged. It costs us no signal — all five signals are observed engine-side (message state,
     agent_state, engine log, origin), none require the agent to report back.
  2. Textual: tell it in the task body not to message anyone. Weaker; it relies on the agent choosing to comply,
     and our own contract says we never rely on that.

  **One caveat on reading the signals.** In `spawn`, workspace materialisation is currently swallowed:
  `materialise(...).await.unwrap_or(None).unwrap_or_else(|| workspace.clone())`. If the clone fails, the agent
  starts anyway with a bare cwd and improvises — so a materialisation failure would present as "it worked, then
  the agent did something odd" rather than as an error. If signal 5 fails under option (B), check the engine log
  before concluding the agent misbehaved. I would rather flag my own swallowed error now than have it cost us the
  first wake. Tightening it is a small follow-up I will take.

## Trigger
PM pulls the wake only after all four SDK slots are filled and pushed. Re-read this file, do not trust a
message summary of it.

## PM DECISIONS (2026-09-07) + prerequisite status

Adopting all of SDK's recommendations:
- Option (B): PATCH `workspaces` onto adversary first (routes clone through the A9 fix, no PAT on disk;
  validates merge-PATCH for real), then start+send.
- Cascade bound: DELETE adversary's `pm:send` wire for the duration (structural; reversible; costs no
  signal — all five are engine-side). Restore it after.
- Agent: adversary (0be41bbb) — smallest blast radius, empty workspace, ephemeral_context:false.
- Drive via the PUBLIC API (SDK's intended path; tests the auth boundary we ship), NOT the engine proxy.

PREREQ STATUS:
1. merge-PATCH deployed: SATISFIED. wheel-host is on commit 330deed (RFC-7386 merge PATCH), status
   SUCCESS, deployed 11:27Z; later docs-only commits correctly did not rebuild.
2. claude auth: SATISFIED (SDK measured authenticated=true, env mode, CLAUDE_CODE_OAUTH_TOKEN on vault).
3. GITHUB_TOKEN: OPERATOR must confirm a CURRENT token is in secrets/GITHUB_TOKEN. Vault values are
   write-only, so neither SDK nor PM can read it. Clone fix has landed, so storing one is now safe. If it
   is empty/revoked, signal 5 (branch lands on origin) fails as a stale credential, not a product gap.
4. owner auth for 6906cadb: OPERATOR-gated. The wake drives via the public API as an OWNER of the
   operator's project; PM must not authenticate as the operator (standing constraint). So the operator
   either issues a session token for PM to drive the ~5 calls, or runs the calls himself with PM's exact
   sequence. Engine-proxy drive is rejected: SDK's guidance is it bypasses the auth boundary we ship.

EXACT SEQUENCE (once 3 and 4 clear), via https://wheel-api-production.up.railway.app, headers
`x-auth-token: <owner jwt>` + `x-project-id: 6906cadb-...`:
  a. DELETE adversary's pm:send wire (cascade bound)
  b. GET adversary node (baseline config)
  c. PATCH adversary {"config":{"workspaces":[{"path":"wheel","git":{"url":".../wheel.git","ref":"main"}}]}}
  d. GET adversary node again, diff -> proves merge kept harness/system_prompt/run_on_startup
  e. POST agents/adversary/start ; POST agents/adversary/send {"body": append-line-to-dogfood-wake-log,
     commit, push to branch dogfood/wake-test-1, do NOT message anyone}
  f. observe 5 signals (read-only, via host proxy is fine for observation)
  g. restore: re-add adversary pm:send wire; delete test branch
Caveat (SDK): materialise errors are swallowed in spawn; if signal 5 fails under (B), read the engine log
before blaming the agent.

## F007 (ADVERSARY, measured 2026-09-07) — shared per-project token; bears on the wake AND gates script-exec
Within a project the auth token is SHARED across all agents (the one-uid-per-project gap, 037/F007). The
wire matrix is enforced correctly against the token, but any agent can present as another — so any agent
can impersonate pm, who holds send to all six. Measured, not assumed. NOT cross-tenant (host root env not
exposed; tenant isolation unaffected); not a regression; not urgent as a standalone.

Bearing on THIS wake: the cascade bound (delete adversary's pm:send wire) stops adversary SENDING to pm,
but does NOT stop it IMPERSONATING pm via the shared token. Honest limit. Wake stays GO because the risk
does not materialise for our case: adversary is our own benign agent, instructed not to message, observed,
and its blast radius is the board's own agents (not cross-tenant, not host). We are not relying on the
wire bound against a hostile agent — we are waking a cooperative one and watching.

Bearing on SCRIPT EXECUTION: this is now a stated PRECONDITION, not later hardening. Script-exec on a
shared-token board means an agent running arbitrary code can impersonate pm and drive the whole board.
Per-node uids/tokens (037/038) must be in SDK's Script-exec scope as a gate, not deferred to M2.
