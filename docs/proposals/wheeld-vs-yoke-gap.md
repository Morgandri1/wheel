# Running this swarm on local `wheeld` instead of YOKE — the measured gap (SDK, 2026-09-07)

PM asked what it would take, so the operator's answer is measured rather than guessed. Everything below is
either `file:line` or a live reading off the production host. Where I could not establish something I say so.

## The headline, in one paragraph

**On communications, `wheeld` is already better than YOKE — materially, not marginally. On compute, it is not
better yet, and the reason is the single feature the project exists to provide.** Messages are sqlite rows
with a `{id, sha256, bytes, state}` receipt and a byte-identical re-read; nothing anywhere truncates a body,
and an over-limit body is refused with a 413 rather than clipped. That removes the entire class of failure
that has cost us four beheaded messages tonight. But **idle parking is not implemented**, so an agent that has
taken one turn holds a `claude` process forever — which is precisely the YOKE behaviour the operator wants to
escape.

## What I measured on the live host, not inferred

One woken agent, an hour after its last turn, queue empty, status `idle`:

```
21088  11    04:36:11   18916 wheel-engine          <- 18 MB, no polling: the engine is genuinely frugal
21088  79    04:03:44  166164 claude --print ... --resume 9d546454   <- 162 MB, STILL RESIDENT
21088 110    04:03:43    1752 wheel mcp-serve
21088 2141      45:09       0 [wheel] <defunct>     <- zombie, PPID 1
21088 2182      44:35       0 [wheel] <defunct>     <- zombie, PPID 1
```

Three facts in that dump:
1. **The engine costs 18 MB and idles at ~0 CPU.** §2's "the engine itself must idle at ~0 CPU" is real.
2. **An idle agent costs 162 MB indefinitely.** Six such agents ≈ 1 GB resident doing nothing.
3. **Zombies accumulate.** `wheel-host` is PID 1 with no reaper, so orphaned grandchildren pile up — two in
   45 minutes from one agent. A slow PID leak, not urgent, but it is there.

## Answering PM's questions in order

### agent↔agent messaging — path implemented; the *last hop* is untested for an agent sender
The chain is complete and sound: `wheel msg` → `/v1/cli/msg` (sender resolved **from the token**, so `type="agent"`
is unforgeable) → sqlite row with sha256 of the original → `deliver` → the single stdin writer. Seven
integration tests cover agent→agent at the CLI plane, including a 200 KiB hostile body and a byte-identical
re-read by the recipient's own token.

**The gap is narrow and specific:** every test that asserts a message reaching a child's *stdin* uses a **user**
sender. `PROGRESS-message-reaches-consumed/*` wires `user-send` and `endpoint-ingress` only. Agent-sender
envelope rendering is proven in wheel-core unit tests, and the composition is proven for user senders — but
nothing asserts the two together. That is the exact producer our swarm would live on. It is a test gap, not a
code gap, and it is cheap to close.

### shared memory — ctx/table/vault all there
ctx read/write, table row read/write/ls/rm/query, vault `secret get`/`list` are all implemented and wired
through the CLI plane. ctx-read, vault-get and table-write have integration coverage; `query`, `ls` and `rm`
are implemented but only negatively tested. Vault writes from an agent are *deliberately* refused. Chest is
M2 and answers an honest 400 — we barely use it, so it does not block.

### wiring — there and enforced, in the right place
The matrix is enforced in the DB layer (`db/board.rs`), not the route, so no API path can bypass it, and it is
exhaustively tested over every from/to/type triple.

### `wheel place` — agreed, a static board sidesteps it entirely
`place` does not exist. It does not need to: `infra/bootstrap-board.sh` already builds **exactly our board** —
six agents, the contract and workflow ctx nodes, the secrets vault, the reports table, and the full wire mesh
including pm↔everyone — through the public API, idempotent by project name, refusing a non-loopback target
unless explicitly allowed. **One caveat that matters (below): it sets no `workspaces`.**

### the CLI as the agent's interface — three gaps, one of which is ours
Implemented and real: `whoami, connections, list, ls, read, write, rm, query, msg, inbox, secret get/list,
tool ls/call, mcp-serve`, `--json` throughout, exit 3 on wire denial.

Missing, and each is a promise the agent has already been given:
- **`wheel run <script>`** — absent, exits 1. The preamble advertises it to every agent at startup.
- **`wheel ctx clear`** — absent from the CLI dispatch, *but the engine route exists and works*. The preamble
  and PROTOCOL both promise it. This is the cheapest fix on the list: one match arm.
- **`wheel inbox --limit/--since`** — documented; the CLI passes `--limit` as a *message id*. Silently wrong
  rather than an error.
- `--reply-to`, `--wait`, and `msg a,b,c` fan-out are M2 and would be missed by a hub-and-spoke swarm where
  the PM messages five agents at once — five sends instead of one.

### local run — a config exercise, with one real constraint
`wheeld` is implemented: one binary running api+host+embedded engines, sqlite only, local email/password auth,
no docker and no Postgres. Its smoke test passes 6/6 from a deliberately scrubbed environment and proves
signup → project → engine reachable through the API in one process.

The constraint is not the engine, it is the machine: children are spawned with `env_clear()` and a small
allowlist, so the laptop must itself provide `claude`, `wheel`, `git`, `gh`, `cargo`, `node`, `pnpm`, `python3`
— everything `Dockerfile.host` installs. And `SANDBOX_BACKEND=process` is **not viable on a laptop**: it needs
root and `/proc` to setuid per project. `wheeld` sidesteps this with an embedded sandbox that gives up
per-tenant isolation on purpose, on the stated grounds that the tenants are all the same person. For our swarm
that is the right trade — but it means one uid for everything, so any agent can read any other's token file.

### robustness vs YOKE — PM's read is correct, and stronger than stated
Nothing truncates. The body is stored verbatim; the only transformation is envelope escaping, which *grows*
the string and is byte-sliced deliberately (an em dash once panicked the escaper and took the board down
through reboots). Over-limit is a clean 413 before enqueue. Undeliverable bodies are quarantined with a
reason; a write failure leaves the message `queued` with `last_error`; a poison message is consumed exactly
once and never loops.

One real defect: the CLI does **not** pre-check the 256 KiB limit despite a comment claiming it does, so a
large `--file` send uploads and then fails — the "discover the limit by failing" that §3c#6 exists to prevent.
A wasted round-trip, not a truncation.

## The honest split PM asked for

### A session or two of deploy + config + confirm
- Run `wheeld` on the laptop; install the toolchain it expects on PATH.
- Run `bootstrap-board.sh` against it — the board already exists as code.
- **Add a `workspaces` block per agent** to the bootstrap (see below).
- Close the agent-sender transcript test gap.
- Add a Makefile target for `wheeld`, and make its smoke test reachable without the docker-gated runner.
- `wheel ctx clear`: one match arm.

### Real missing engine work, and it is short but load-bearing
1. **Idle parking (§3c#14).** The config field, the accessor, the default, the `Parked` status and a
   wheel-core test *named* `an_agent_parks_after_the_default_idle_timeout_unless_it_says_otherwise` all exist.
   **The engine never reads the field.** There is no `Idle → Parked` timer. This is the single feature that
   distinguishes Wheel from YOKE on compute, and it is the one that is missing.
2. **Per-host concurrency cap + fair queue.** Specified in two documents with a default of 32. Nothing
   implements it — no env var, no semaphore, no queue. Six agents woken together all spawn at once, and
   `wheeld`'s embedded sandbox applies no rlimits at all.
3. **Operator interface.** The `wheel` binary requires a *node* token, so a human cannot drive the board with
   it; `wheel login`/operator mode is M3. Today the human's surface is the web app (`npx wheel-web`) or curl
   against the send route. Workable, not pleasant, and worth knowing before the operator expects a CLI.

### Config, not engine, but it is the one I would fix first
`bootstrap-board.sh` sets no `workspaces`, and `WHEEL-ON-WHEEL.md` tells agents to `git clone` into `$HOME`
themselves. That improvisation is exactly what wrote a live PAT into a `.git/config` and filled a 4.6 GB
volume. The good path is built and proven in production today — one shared object store per repo, a
`git worktree` per agent, credentials via `GIT_ASKPASS` and never on disk; measured this morning at 4.1 MB of
store plus 7.1 MB per worktree against ~1.9 GB for a full clone. The bootstrap simply does not use it.

Related, and it will bite on a laptop: `$HOME` is per node, so every agent re-downloads the same toolchain
caches. Measured in production at 1.76 GB of byte-identical pnpm store across two agents.

## What I would tell the operator

Moving this swarm to local `wheeld` is **mostly a configuration exercise, and it fixes our comms problem
immediately** — durable messages, receipts, byte-identical re-reads, no beheading. The board is already
expressible as code and the runtime already boots from a scrubbed environment.

It will **not** fix the resource problem that motivated the move, and it would be an unpleasant surprise to
discover that after switching. Six agents on a laptop would hold six `claude` processes indefinitely, with no
concurrency cap and no rlimits. Wheeld is still strictly better than YOKE here — §3c#13 guarantees one process
per agent, enforced by a test, where YOKE spawns one per message — but "one per agent, forever" is not the
frugality that was promised.

**My recommendation: implement idle parking first, then move.** It is the smallest change on this list, it is
the difference between the two systems on the axis the operator actually cares about, and every day we run
without it is a day the pitch and the runtime disagree.
