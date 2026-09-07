# Deploy/restart robustness — what is ALREADY TRUE (SDK, 2026-09-07)

PM asked for the current-behaviour read, not a build. Answers are from code with `file:line`, and the
headline is corroborated by production logs. Where I could not establish something I say so rather than
estimate.

**Short version: less is already true than the shape you gave the operator.** Session resume and task
persistence are real and solid. Graceful drain is not — the engine's shutdown handler exists but **never runs
in production**. And "the task is not lost" is currently false in the specific case that matters: an engine
killed mid-turn loses that message silently.

---

## THE HEADLINE — the engine's graceful shutdown never executes

The chain on a Railway container swap:

1. Railway/Docker sends SIGTERM to **PID 1 only**. PID 1 is `wheel-host` (`entrypoint.sh:44` `exec`s it, and
   `/proc/1/cmdline` in production confirms it). Signals do arrive — `exec` on every path, no shell, no
   `STOPSIGNAL`, so that half is correct.
2. **`wheel-host` installs no signal handler at all.** `serve_on` calls `axum::serve(...)` with no
   `.with_graceful_shutdown` (`crates/wheel-host/src/lib.rs:529`). A process running as **PID 1 with no
   handler does not get default-killed — the kernel discards the signal.** So the host ignores SIGTERM
   entirely.
3. The grace window expires and SIGKILL arrives at PID 1. The container is torn down and every process in it
   dies at once.
4. **The engine therefore never receives SIGTERM.** Its handler (`crates/wheel-engine/src/lib.rs:151-167`) is
   real and correct, and is dead code in this deployment.

**Evidence, not inference.** The engine's handler logs `"SIGTERM received, shutting down"` and then
`"shutdown complete"`. I read the full logs of the previous host deployment (`0040cc0e`): boot is richly
instrumented — reconcile restored 1 of 1 in 204 ms, "agents parked on startup, count 6" — and the shutdown
side is **`Stopping Container` with not one line from either process**. Neither string appears. That is what
a handler that never runs looks like.

Consequence: harness children are SIGKILLed with no chance to flush, every in-flight turn dies mid-write, and
`kill_on_drop(true)` (`supervisor/mod.rs:587`) never gets to run either.

---

## The five questions

### 1. Does the engine drain on a container swap? **NO.**
Not because drain is unimplemented — because the signal never reaches it (above). Even if it did, the handler
only stops the HTTP server: nothing awaits in-flight turns, nothing stops children, nothing flushes sqlite.
Durability comes from the sqlite pragmas, not from a shutdown step.

`wheel_core::spawn::SHUTDOWN_DEADLINE_SECS = 15` — the value PROTOCOL.md and ARCHITECTURE §4b quote as the
engine's contract — **has no reader anywhere in the tree.** The 15 s is enforced only from outside, by the
host, and only on an explicit `POST /stop` (`wheel-host/src/sandbox/process.rs:417-430`: SIGTERM, wait 15 s,
`kill()`). On a container swap that path is not taken.

So: in-flight turns are lost today, and this is the first thing to build.

### 2. Is an in-flight message re-delivered on boot? **NO — it is stuck in `delivered` forever.**
The requeue machinery exists (`db/messages.rs:288-317`) but has only two production callers, both in-process:
`supervisor/mod.rs:1188` (`reap`, reached only when the child's stdout closes **while the engine is alive**)
and `:1005` (auth failure). Boot runs `start_configured_agents` (`supervisor/mod.rs:1295-1327`), which parks
`run_on_startup` agents and calls `deliver` — and `deliver` gates on `has_queued`, which counts
`state = 'queued'` only (`db/messages.rs:169`). A `delivered` row is invisible to it.

`in_flight` cannot help: it is an in-memory field on `struct Running` (`supervisor/mod.rs:127`), never
persisted, `None` on every fresh start. The 041 deadline is a per-start timer, not a boot sweep.

**Two second-order findings you should have, because they make this worse than "a message is lost":**

- **The stall detector is blind to exactly this row.** `agents_with_work_older_than` — which backs `/healthz`
  — explicitly excludes any agent having a `delivered` row (`db/messages.rs:152-155`). The reasoning is sound
  for the healthy case (an agent mid-turn is progressing, not stalled) and wrong after a crash, where the row
  is a headstone rather than a turn. So a permanently wedged agent reports **healthy**, and every message
  queued behind it is hidden from the stall report too. This is the green-gate-describes-nothing shape again.
- **An agent without `run_on_startup` does not come back at all.** `deliver` starts an agent only when its
  persisted status is `Parked` (`supervisor/mod.rs:1351`), and only `run_on_startup` agents are forced to
  `Parked` at boot. One that was `running` when the engine died keeps that stale status, so a new message
  enqueues and nothing starts. It needs a manual `POST /v1/agents/:id/start`.

### 3. What protects a re-run turn from double-applying? **NOTHING. And this is a decision, not a bug.**
There are no idempotency keys, no dedupe, no side-effect ledger. "Consumed exactly once" is a forward-only
state machine (`wheel-core/src/message.rs:107-118`, `db/messages.rs:239-252`) — it prevents the *state* from
moving backwards; it says nothing about side effects.

**The tension is already in the tree, and it is the crux of your question.** `requeue_undelivered` is
documented as *"the ONLY backwards transition, and deliberately not routed through `advance`"*, and its safety
argument is explicit: it is safe *"precisely because delivery did NOT happen in any meaningful sense: the
bytes went into the pipe of a process that then exited without producing a `result`, so no turn ever ran"*
(`db/messages.rs:288-306`).

That premise holds for the case it was written for — a harness that died at startup. It does **not** hold for
the crash case you are asking about: the engine can die after the agent has already pushed a commit, written
files, or messaged another agent, but before `result` arrives. `reap` blanket-requeues every `delivered` row
with the reason *"the harness exited before this message could be processed"* — an assertion the code cannot
verify.

So the two halves of the system already disagree: **`reap` gives at-least-once with no double-apply guard;
boot gives at-most-once by dropping the row.** Neither is the documented contract, which says never redeliver.

**The trap to avoid:** the obvious fix for question 2 — a boot-time requeue sweep — converts a silent-loss bug
into a **silent double-apply** bug, including re-sending messages to other agents and re-running tool calls.
It is a five-line change that looks like a fix. It needs a dedupe layer to land on first, or an explicit
ruling that at-least-once is what we want and agents must tolerate replay.

### 4. Is the session store persistent? **YES — your statement to the operator is correct.**
Verified in production: `/data` is a real ext4 mount, and both `run/` and `creds/` sit under
`/data/projects/<id>/`. Nothing is in `/tmp` — the host even redirects `TMPDIR` into the project tree
(`sandbox/process.rs:122`) to avoid a shared-`/tmp` cross-tenant channel.

- session id: `agent_state.session_id` in sqlite on the volume (`db/schema.sql:54`)
- harness config/`HOME`: `<data>/creds/<node>` (`supervisor/mod.rs:540-541`, `harness/claude.rs:56-69`)
- run dir (token, prompt, mcp.json): `<data>/run/<node>` (`config.rs:161-163`)
- `--resume` is read from sqlite and passed on argv (`supervisor/mod.rs:452-454` → `harness/claude.rs:49-52`)

One caveat: nothing checks the session still exists before passing `--resume`; a stale id fails at the CLI.

### 5. Grace window? **I could not establish it, and I will not quote a number I cannot source.**
Our `infra/railway/settings.json` sets no override, so it is the platform default. It is also **moot until
question 1 is fixed** — the host ignores SIGTERM regardless of how long the window is, so today the window
buys us exactly nothing.

It is one measurement to settle: log on SIGTERM in the host, redeploy, and time the gap to process death.
That measurement is worth taking *as part of* the drain work rather than before it.

---

## Already true vs needs building

**Already true:** signals reach PID 1; session id and creds survive a swap and `--resume` uses them; the host
reconciles and restarts projects on boot (204 ms in production); leftover processes are reaped before the next
engine start (`sandbox/process.rs:71-103`); a message that was never delivered stays `queued` and drains on
the next start.

**Needs building, in dependency order:**
1. **Host SIGTERM handler** — without it nothing else in this list can run. Smallest real fix, largest effect.
2. **Engine drain** — stop accepting, stop starting new turns, await in-flight with a bound, then exit. Wire
   `SHUTDOWN_DEADLINE_SECS` to an actual reader so the documented contract becomes true.
3. **A boot decision on `delivered` rows** — and the dedupe question in Q3 answered *before* the sweep is
   written, not after.
4. **Un-blind the stall detector** for the crashed-mid-turn case, so a wedged agent stops reporting healthy.
5. Restart-or-report agents left `running` by a crash.

None of this changes the wake, and I am not building any of it now.

---

## One thing outside the question, since I found it

`CODEX_HOME` appears only in comments. There is exactly one `Harness` implementation, `ClaudeDriver`
(`harness/mod.rs:11`), hardcoded in `Supervisor::new` (`supervisor/mod.rs:267`). If the operator has been told
Codex agents are available today, that is not accurate — it is M2.
