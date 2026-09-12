# Proposal: auto-update — deliberate, gated, drained runtime updates

Status: **proposed; needs a PM ruling on R1–R8 below.** v1 is implemented on `sdk/auto-update` exactly as
written here, so the ruling can be made against running code rather than a sketch.
Author: SDK+API builder (auto-update). Date: 2026-09-11, revised after #63 (headless-first) and #65 (the
VPS kit) landed — both changed this design, and the revision is marked where they did.

## Why

Operator directive, verbatim:

> every time a wheel cli command is run by an agent they'll be prompted to update any component if
> pertinent; ideally, this simply `git fetch origin/main` and pull latest changes before restarting the
> engine or whatever. this will allow the wheel cloud board to continue developing itself without much
> (or ideally any) interaction on my side.

`cloud-board-handoff.md` names what stands between the board and developing its own runtime: every engine
merge redeploys `wheel-host` and kills the board's agents mid-turn. The fix is deploy-drain or DECOUPLING,
where engine updates are deliberate rather than every-merge. **This feature is that decoupling.** The
runtime moves only when someone asks, or at a quiet moment in `auto` mode, and never while a turn is running.
`deploy-resume-and-drain.md`'s ruling holds: blind replay of a killed turn is not acceptable. So an update
waits for a quiescent point and never kills a turn to get one.

## What the code already says (read before the design)

| Fact | Where | Consequence for this design |
|---|---|---|
| The engine bakes its commit at compile time as `WHEEL_BUILD_SHA`; unstamped builds report `unknown`. | `wheel-engine/src/api/mod.rs` `build_id()` | The running SHA already exists. The updater **stamps every build it makes**, so after the first update the running SHA is exact. An unstamped first install falls back to the checkout's `HEAD` and is labelled `assumed`. `sdk/agent-grid-engine`'s `GET /v1/engine` (`EngineInfo.build`) will read the same constant; this branch makes `build_id` public rather than inventing a second one. |
| **#63 already built the drain.** `Supervisor::shutdown` sets `closing` (nothing starts, `pump_queue` writes nothing), waits up to `SHUTDOWN_DRAIN` (20 s) for `turns_in_flight()` to reach zero, SIGTERMs each agent's process group, consumes anything still running as `INTERRUPTED_BY_SHUTDOWN` rather than requeueing it, and leaves every agent **parked** with its session. | `supervisor/mod.rs` | **The update reuses this and adds nothing of its own.** It only has to make that shutdown land on a board where nothing is in flight. So: `pause_turns()` sets the SAME `closing` flag, the updater waits for `busy_agents()` to empty with its own much longer bound, and then the daemon's ordinary shutdown runs. A second "draining" flag was written and deleted: the first time it disagreed with `closing`, a turn would start inside a drain. |
| wheeld runs each engine as a task in its own process, and every agent child runs as wheeld's uid. | `wheeld/src/embedded.rs`, `SHARED_UID_WARNING` | The updater can reach every engine in-process (no new HTTP surface). But **an agent on wheeld can already rewrite wheeld's binary**. See the threat model: on shared-uid wheeld the controls stop confused agents and remote pushers, not a malicious local agent. |
| `wheeld` stops the API, then `sandbox.shutdown_all()` stops every engine, which stops every agent's process group (#63). `wheeld.service` uses `KillMode=mixed`, `TimeoutStopSec=35`; compose gives `stop_grace_period: 35s`. | `wheeld/src/lib.rs`, `infra/vps/` | The update needs no shutdown of its own: it ends serving early, and the existing path does the rest. |
| Boot parks `run_on_startup` agents and resumes those with queued work; a **parked** agent without `run_on_startup` that has queued work is not resumed until the next message arrives. | `start_configured_agents` | A message queued during a drain would strand behind a parked agent. Boot now also resumes every `Parked` agent with **`queued`** work. It never touches a `delivered` row, so the effectively-once ruling is untouched. |
| Railway deploys `wheel-host` on every push to main that matches its watch paths. | `infra/railway/README.md` | A Railway driver and deploy-on-push cannot both manage one runtime (see "Drivers"). |
| **#65 pre-wired this lane's hook points.** `install.sh --updatable` creates `/opt/wheel/src` (checkout), `/opt/wheel/bin` (binaries), `/var/cache/wheel/update` (staging, off the data dir), hands the first two to the `wheel` user, adds them to `ReadWritePaths`, and documents `WHEEL_UPDATE_RESTART=exit` → exit 75 against `Restart=on-failure`. | `infra/vps/install.sh`, `systemd/wheeld.service`, `README.md` | v1 targets this layout exactly, and uses those variable names unchanged. Nothing in `infra/` needs to change for the systemd path. |
| Self-hosted deployments authenticate the harness with **OAuth plus refresh**, and the refreshed credential is written by the harness child into the node's config dir. | `auth.rs` (`expires_at`), `docs/proposals/wheel-harness-auth.md` | A refresh happens *during a turn*. Waiting for quiescence is therefore also what keeps an update off a live credential refresh — a plain restart has no such guarantee. Nothing in the update path reads, writes or revokes a harness credential. |
| `main` has no branch protection. | ARCHITECTURE §1 | "CI green on main" is necessary but not sufficient. See T1. |

## Design

### Policy: deployment-level, never settable through the API

The pattern is `WHEEL_HARNESS_AUTH`: an env var on the process that runs the runtime, read once at boot,
invisible to and unreachable from every project API and every sandbox. A policy a project owner or an
agent could flip would not be a policy.

| Variable | Values / default | Meaning |
|---|---|---|
| `WHEEL_AUTO_UPDATE` | `off` \| `prompt` \| `auto` — **default `off`** | `off`: no notice, and every request surface refuses. `prompt`: notice plus agent/operator request. `auto`: `prompt`, plus apply at quiescence without a request. Anything else fails boot and names the variable. |
| `WHEEL_UPDATE_REPO` | path, **required unless `off`** | The source checkout to fetch and build from. Remote is always `origin`, branch always `main`; neither is configurable, to keep the attack surface small. |
| `WHEEL_UPDATE_GITHUB_TOKEN` | token, optional | Reads GitHub check-runs for the CI gate. Without it the gate is **unverifiable**: nothing applies, and boot, every notice and `wheeld update` say so. |
| `WHEEL_UPDATE_GITHUB_REPO` | `owner/name`, optional | Defaults to what `origin` points at on github.com. |
| `WHEEL_UPDATE_REQUIRED_CHECKS` | comma list, default `make check` | Check-run names that must be present and green, in addition to "every GitHub Actions run on the commit is green". |
| `WHEEL_UPDATE_BIN_DIR` | default: directory of the running `wheeld` | Where `wheeld` and `wheel` live. Both must be present; a missing one fails boot. |
| `WHEEL_UPDATE_STAGING` | default `$XDG_CACHE_HOME/wheel-update` or `~/.cache/wheel-update` | Where builds happen. Refused if inside the data dir: builds stay **off the data volume** (the 2026-09-06 outage was a `target/` on the volume). Its `target/` is a private `CARGO_TARGET_DIR`. |
| `WHEEL_UPDATE_FETCH_SECS` | default 300, minimum 60 | Minimum interval between fetches, periodic and lazy alike. |
| `WHEEL_UPDATE_DRAIN_SECS` | default 600 | How long to wait for in-flight turns before abandoning an attempt. |
| `WHEEL_UPDATE_HEALTH_SECS` | default 60 | How long a new binary has to prove healthy before it is rolled back. |
| `WHEEL_UPDATE_RESTART` | `exec` (default) \| `exit` | Re-exec in place (same pid; systemd sees nothing) or exit 75 for a supervisor with `Restart=always`. |

### Components and pertinence

Changed paths between the running SHA and `origin/main` are mapped to components. **First match wins.**

| Component | Paths |
|---|---|
| `ci` | `.github/**`, `qa/check.sh`, `qa/tools/**`, `qa/*.json`, `Makefile` |
| `core` | `crates/wheel-core/**`, `crates/wheel-sqlite/**`, `Cargo.toml`, `Cargo.lock`, `rust-toolchain*` — touches everything |
| `engine` | `crates/wheel-engine/**` |
| `cli` | `crates/wheel-cli/**` |
| `host` | `crates/wheel-host/**`, `crates/wheeld/**` |
| `api` | `crates/wheel-api/**` |
| `web` | `web/**` |
| `docs` | `docs/**`, `redteam/**`, `*.md` |
| `other` | anything else (`docker/`, `infra/`, `qa/` fixtures, …) |

A change is **pertinent** when it touches a component this deployment runs. wheeld runs
`{core, engine, cli, host, api}`. A docs-only, web-only or `other`-only range is not pertinent: no notice,
no request, no apply.

`ci` is the one new idea here (R2). CI is defined by the commit it gates, so a commit that edits
`.github/workflows/ci.yml` can make itself green. A range touching `ci` is **never applied because an agent
asked, or by `auto`**: the notice reads `blocked: CI definition changed — operator must run
\`wheeld update\``, and only the operator path applies it.

### Check

- `git fetch origin main` in `WHEEL_UPDATE_REPO`, at most once per `WHEEL_UPDATE_FETCH_SECS`. It runs on a
  periodic tick and lazily on any `/v1/cli/*` call. The lazy call never blocks the response: it only starts
  a background fetch if the interval has passed, and a fetch already running absorbs it. Git runs with
  `core.hooksPath=/dev/null`, `core.fsmonitor=false` and `GIT_TERMINAL_PROMPT=0`, so a planted hook or
  fsmonitor in the checkout's own config does not execute on fetch.
- The target is `origin/main`. From running SHA to target, the check computes:
  - fast-forward: `merge-base --is-ancestor running target`
  - commit count
  - changed paths, then components
  - a clean checkout: no staged, unstaged or untracked changes
  - the checkout's `HEAD` is an ancestor of the target
- It asks the CI gate about the **target** SHA, then publishes one cached status. `notice()` is a read of
  that cache and costs nothing.

### CI gate

`GET /repos/{repo}/commits/{sha}/check-runs` with the deployment token.

**Green** requires all three:
- at least one run from app `github-actions`
- every such run `completed` with conclusion `success`, `neutral` or `skipped`
- every name in `WHEEL_UPDATE_REQUIRED_CHECKS` present and `success`

Any run still in progress is **pending**, and the next check retries. Any failure is **red**. No token, a
non-GitHub origin, or an API error is **unverifiable**. That is never treated as green: it is said on boot
at error level, in every notice and by `wheeld update`.

Only **check-runs from the GitHub Actions app** count. Commit *statuses* are ignored entirely: any PAT with
`repo:status` can post one. A check-run from any other app is ignored too: a GitHub App with `checks:write`
could forge one.

### Prompt: the notice

- Every `/v1/cli/*` response carries `x-wheel-update: <compact JSON>` while there is something pertinent to
  say: available, requested, applying, blocked, or failed and rolled back. The JSON holds only closed enums,
  hex SHAs and integers:
  `{"state":"available","running":"abc1234","target":"def5678","components":["engine","cli"],"commits":7}`.
- The `wheel` CLI prints **at most one line, to stderr, never stdout**:
  ``wheel: update available abc1234→def5678 (engine, cli; 7 commits) — run `wheel update` at a safe point``.
  `--json` gets a `wheel_update` field on the object instead, and no stderr line.
- The MCP bridge (`wheel mcp-serve`, what agents actually use) appends the same line as a second text item
  to a tool result, **once per bridge session per target SHA**. A notice on every tool call would bill the
  model for it every time. It also exposes an `update` tool (`{"action":"request"|"status"}`), listed only
  when the deployment has a policy other than `off`.
- **No git-derived text reaches an agent.** No commit subjects, author names or paths: they are chosen by
  whoever pushed, and an agent reading them in its tool output is a prompt-injection channel (T6). The
  types enforce this: SHAs parse as hex or the notice is dropped.

### Request

- `wheel update` (CLI), the MCP `update` tool, and `POST /v1/cli/update` (the API both use) **record** a
  request and return 202 immediately, because the requester is mid-turn.
- The requester must be an **agent** node. A script is refused: an endpoint can trigger a script, so a
  script request would hand the public internet a restart lever.
- `wheeld update` is the operator's hand path: it records an operator request in `<data>/update/state.json`,
  which the daemon picks up on its next tick. `wheeld update --status` prints the cached status.
- With the policy `off`, all four refuse. The CLI and MCP get `403 update_disabled`, naming the variable.
  `wheeld update` exits 2.
- A second request while one is pending coalesces into it. A request for nothing pertinent says so and
  records nothing.

`wheel update <agent> --prompt …` is reserved in ARCHITECTURE §3e for editing an agent (M2, unbuilt). v1
takes **zero positional arguments** as the runtime update and refuses any positional argument with a
pointer, so §3e's grammar stays available (R3).

### Apply: in order, each step a gate

1. **Fetch** (the rate limit does not apply to an apply).
2. **Verify**: fast-forward only, clean checkout, CI green, SHA not marked bad, `ci` untouched unless the
   operator asked. Refusal records the reason and posts it to the requester.
3. **Build** from `git archive <target>`, the exact tree and nothing else from the checkout, into
   `<staging>/src-<sha>`. The command is `cargo build --release --locked -p wheeld -p wheel-cli` with
   `CARGO_TARGET_DIR=<staging>/target` and `WHEEL_BUILD_SHA=<target>`. It runs before the drain, so the
   board is only paused for the fast part.
4. **Smoke**: the new `wheeld --version` must exit 0 and report the target SHA, which proves the right
   commit was built. `wheel --help` must exit 0.
5. **Drain**: every engine stops *starting* turns. The daemon polls, bounded by `WHEEL_UPDATE_DRAIN_SECS`,
   until no agent has a turn in flight.
   - **On timeout it resumes delivery**, records `drain timed out (agents: a, b mid-turn)`, and tells the
     requester. It does not kill anything. Messages that arrived meanwhile stay `queued` and drain normally.
6. **Park and shut down**: every idle agent is parked, which keeps its session and stops its process.
   Nothing is mid-turn by construction. Then graceful shutdown: stop serving the API, stop every engine
   (the headless-first seam).
7. **Swap**: `wheeld` and `wheel` are copied into `BIN_DIR` as `.new`, the current ones hard-linked to
   `.prev`, and the new ones `rename(2)`d over. That is atomic per file. A **pending marker**
   `{from, to, requester, attempts}` is written to the state file.
8. **Restart**: re-exec the new `wheeld` with the same argv, or exit 75 under `WHEEL_UPDATE_RESTART=exit`.
9. **Boot**:
   - The new binary sees the marker, increments `attempts`, and starts a watchdog on a plain OS thread, so
     it fires even if the async runtime is wedged.
   - Agents come back parked with their sessions. Any with queued work resume.
   - Health means: the API answers `/healthz` on its own listener, the host has reconciled, and every
     project that was running before is running again.
10. **Record**: on health, the marker is cleared, a history row `updated abc1234→def5678` is written, and a
    `system`-type message goes to the requester. After a successful restart the checkout is fast-forwarded,
    so the operator's tree matches what runs.

### Rollback

- **Unhealthy within `WHEEL_UPDATE_HEALTH_SECS`**: the watchdog renames `.prev` back over both binaries,
  marks the target SHA **bad**, and re-execs the previous binary. The previous binary then records the row
  and tells the requester. A bad SHA is never retried automatically and never prompted again. A *newer*
  SHA is still eligible.
- **Crash before health**: the next boot finds the marker with `attempts ≥ 1` and does the same before
  anything else runs. "Before anything else" is meant literally: the decision is made on the way into
  `cli_main`, before the tokio runtime is built and before the data directory, the API's
  configuration or its migrations are touched — each of which can fail on a bad build, and none of
  which could then reach a rollback. The residual is a build that crashes in argument parsing itself,
  which the smoke test (`wheeld --version` on the new binary) already exercises before any swap; past
  that, systemd's `StartLimitBurst` stops the flapping and `install.sh --rollback` is the operator's
  recovery.
- **Circuit breaker**: three failed attempts (any SHA) in 24 h suspend `auto` and agent requests until the
  operator runs `wheeld update`.

### Drivers

```rust
pub trait UpdateDriver: Send + Sync {
    fn name(&self) -> &'static str;
    fn fetch(&self) -> Result<()>;
    fn target(&self) -> Result<String>;                      // the tip we would move to
    fn inspect(&self, running: &str, target: &str) -> Result<Inspection>;  // ff?, clean?, commits, paths
    fn build(&self, target: &str) -> Result<Staged>;
    fn smoke(&self, staged: &Staged, target: &str) -> Result<()>;
    fn swap(&self, staged: &Staged) -> Result<()>;           // keeps the previous artefact
    fn rollback(&self) -> Result<()>;
    fn settle(&self, target: &str) -> Result<()>;            // e.g. fast-forward the checkout
}
```

- **Driver #1, `source`** (v1): git plus cargo, as above.
- **Railway** (later): build and swap become "trigger a deploy of SHA X" through the Railway API, and
  rollback becomes "redeploy the previous deployment". **Deploy-on-push must be off** for any service this
  driver manages. With both on, every merge redeploys under the drain and the two fight: one drains while
  the other SIGKILLs. The driver should refuse to start while the service has a GitHub trigger, reading it
  through the same GraphQL `apply-settings.sh` uses.
- **Docker** (later): build becomes `docker pull <image>@<digest>` where the digest is the one CI pushed
  for that SHA. Swap is a container replace. Rollback is the previous digest. Watchtower-style auto-pull
  must be off for the same reason.

### Composition with #63's shutdown — one mechanism, two callers

This is the part the implementation changed most, and it made the feature smaller.

| | `Supervisor::shutdown` (#63) | An update (this proposal) |
|---|---|---|
| Sets | `closing` | the same flag, via `pause_turns()` |
| Waits | 20 s for turns in flight | `WHEEL_UPDATE_DRAIN_SECS` (default 600 s) for `busy_agents()` to empty |
| If the wait runs out | SIGTERM anyway; the turn is consumed `INTERRUPTED_BY_SHUTDOWN`, never requeued | **aborts the update**: `resume_turns()`, delivery comes back, the requester is told who was busy, and it is retried later |
| Ends with | every agent parked, session kept | handing the daemon a build; the daemon then runs exactly the shutdown on the left, which now finds nothing in flight |

The two are not alternatives: an update *ends* in that shutdown. What the update adds is the guarantee that
the shutdown arrives at a quiet board, so its 20 s drain has nothing to drain and its interrupt branch is
never reached. **The difference in one line: a shutdown must stop, so it may interrupt a turn; an update may
wait, so it never does.**

`resume_turns` is the only thing that clears `closing`, and only on the abort path — `shutdown` keeps it
terminal.

### The target shape is native systemd, and the seam with `api/wheeld-production`

Native `wheeld` under systemd is the default production shape going forward, with Docker demoted.
That is the shape this lane builds for, and it is the one `infra/vps/install.sh --updatable` already
creates: `/opt/wheel/src` (checkout), `/opt/wheel/bin` (binaries), `/var/cache/wheel/update`
(staging, off the data directory).

**The split with `api/wheeld-production` (PR #67), agreed with that lane:**

| | `api/wheeld-production` | this lane |
|---|---|---|
| Owns | the **operator-initiated** lifecycle: first install, a deliberate `--ref` upgrade, and rolling that back. Runs as root, from outside the daemon. | the **daemon-initiated** lifecycle: noticing `main` moved, the CI gate, the drain, the swap, the health check and the rollback. |
| Files | `infra/vps/**`, `docs/proposals/wheeld-native-production.md` | `crates/**`, `docs/proposals/auto-update.md` |

No file is touched by both. The seam is a filesystem and process contract, and both sides now pin it
with tests:

- `/opt/wheel/src`, `/opt/wheel/bin`, `/var/cache/wheel/update`, and the `WHEEL_UPDATE_*` names.
- **`.prev` is one artefact with one meaning**, written the same way by both: `.new`, hard-link the
  current generation to `.prev`, `rename(2)` over. (That lane's `install.sh` used to `mv -f` and keep
  no `.prev`, which silently destroyed this lane's rollback point; it adopts the swap shape above.)
- **Exit 75** restarts the unit (`Restart=on-failure`, no `SuccessExitStatus=`).
- **Neither lane writes `WHEEL_AUTO_UPDATE`.** It is the operator's, in `wheeld.local.env`, off
  unless set.
- `StartLimitIntervalSec=300` / `StartLimitBurst=10` is a **shared constant**. One update attempt
  costs at most two restarts (install, and a rollback if the health check fails), or three in the
  crash case; `WHEEL_UPDATE_COOLDOWN_SECS` (default 600) bounds attempts to one per ten minutes for
  `auto` and agent requests. So a legitimate sequence cannot approach ten restarts in five minutes,
  and the limit does its intended job of catching a crashloop.

**Docker/compose is refused rather than half-supported.** In `docker/Dockerfile.wheeld` the daemon
runs `USER 10001` while `/usr/local/bin/wheeld` is root-owned, so it cannot replace its own binary,
and only `/data` is a volume so a swap elsewhere would not survive `docker compose up --build`. v1
therefore fails boot there with a message naming the variable (`WHEEL_UPDATE_BIN_DIR` must contain
both binaries and not be writable by other users) instead of appearing to work. Giving the compose
path self-update needs a bin volume and an entrypoint change — an `infra/`/`docker/` decision, not
this lane's (**R8**).

### A live credential refresh, and why quiescence is what protects it

Self-hosted deployments authenticate the harness with **OAuth plus refresh, not API keys**. The
refreshed credential is written by the harness child itself, into the node's own config dir, and it
happens **during a turn** — that is when the harness is making requests.

So the protection is not a lock, it is the wait:

- An update proceeds only when **no agent is mid-turn**, so there is no turn in which a refresh
  could be in flight. A plain `systemctl restart` has no such guarantee; this is a property an
  update has and a restart does not.
- Nothing in the update path reads, writes, moves or revokes a harness credential. The swap touches
  `WHEEL_UPDATE_BIN_DIR` only; credentials live under the data directory and are never staged,
  archived or rolled back.
- `git archive` builds from the target commit's tree alone, so no file from the checkout — and
  nothing from any credential store — reaches the build.
- Sessions survive: #63's shutdown leaves every agent **parked** with its `session_id`, and boot
  resumes it, so an agent authenticates after an update exactly as it did before one.

A test asserts the stored credential is byte-identical across a pause that waits out a turn.

### Also out of scope for v1

- **wheel-web**: pinned to the API's version; it would check its own npm version and restart its server. It
  has no turns, so no drain.
- **Multi-tenant wheel-host**: the in-process `EngineControl` becomes the same operations as engine
  control-plane routes, called by the host with each engine's secret. Tenant agents get the notice only if
  the operator opts in, and never the request (T5).

## Threat model (ADVERSARY)

The feature lets code that an untrusted agent may have written, pushed by an agent, trigger a
self-modifying restart of production. Actors:

| | Actor | Capabilities |
|---|---|---|
| A1 | Anyone who can land a commit on `origin/main` (a compromised PR, a board agent with push rights, a stolen token) | chooses the code |
| A2 | A confused agent: benign, buggy, looping | calls `wheel update` in a loop, mid-turn |
| A3 | A malicious agent on **shared-uid wheeld** | arbitrary code at wheeld's uid |
| A4 | A malicious tenant agent on a **multi-tenant host** | arbitrary code at its own uid, not the host's |
| A5 | A network attacker | between the checkout and GitHub |
| A6 | Someone who can post to the GitHub API for this repo without being CI | forged checks |

| # | Threat | Control | Residual |
|---|---|---|---|
| T1 | **Supply chain**: A1 lands malicious code on main and it deploys. | Only fast-forwards to `origin/main`. Only SHAs whose GitHub Actions check-runs are all green and include the required names. Build from `git archive` of that exact SHA with `--locked`. Smoke proves the binary is that SHA. | **CI green is what the commit says CI is.** Hence `ci` (T1b). With no branch protection on main, an agent with push rights *is* a deployer in `auto` mode, **by design of a self-developing board**. Recommend (R6): required reviews plus required checks on main before enabling `auto` on any board whose agents can push. |
| T1b | A1 edits `.github/workflows/ci.yml` or `qa/check.sh` so its own commit goes green. | Any range touching `ci` is refused for agent and `auto` triggers; only the operator's hand path applies it. | An operator who runs `wheeld update` without reading the diff. |
| T1c | A6 forges a green check (commit status, or a check-run from another app). | Statuses ignored; only app `github-actions` counts; required names must be present. | Someone able to run *our* workflows with altered inputs, which is the T1b surface. |
| T1d | A5 serves a different `origin/main`. | Fetch over the transport `origin` uses (https/ssh, authenticated). The CI gate asks GitHub about the SHA we fetched, so a substituted tree whose SHA GitHub never ran CI on is unverifiable and refused. | A5 who is also A1. |
| T2 | **Who can request.** | Agents (not scripts: ingress-triggered) through their node token; the operator through `wheeld update`. Nobody chooses the target: a request is "move to the green tip of main at a quiet moment", nothing more. Policy `off` refuses every surface. | A2/A3 can cause restarts onto code already on main. Bounded by T3. |
| T3 | **Update loops / DoS**: request floods, restart loops, re-trying a broken SHA, a CLI storm driving fetches. | One apply at a time, and requests coalesce. A 10-minute cooldown between agent/auto attempts. Bad SHAs are never retried. A 3-failures-in-24h breaker suspends `auto` and agent requests. Fetch is rate-limited and lazy calls never block. The notice is a cache read. Drain timeout resumes rather than kills. | An agent with push rights can push one commit per cooldown and request each. That is a restart every 10 minutes at quiescent points only, visible in history. |
| T4 | **Rollback**: a bad binary bricks the board. | Previous binary kept. The OS-thread watchdog rolls back on missed health; the next boot rolls back on a crash. The bad SHA is suppressed. | A binary that passes health and is subtly wrong. That is what CI and review are for; the history row and `.prev` make a manual rollback one `mv`. |
| T5 | **Multi-tenant must-nevers (A4)**. An agent must never: (a) make the host restart other tenants' engines on demand; (b) choose, supply or delay the code the host runs; (c) read the update token, repo path or state; (d) see another tenant's requests or history; (e) defeat another tenant's drain, i.e. keep the host draining forever. | v1 is wheeld only. On wheel-host: no tenant agent may request (a); target fixed to green main (b); config is host env, never in an engine's env (c, following `WHEEL_HARNESS_AUTH`); history is host-local (d); drain is bounded and abandons rather than waits forever (e). | One tenant's long turn delays an operator's update, and the drain gives up. That is the intended trade: a turn is never killed for an update. |
| T6 | **Prompt injection through the notice.** | Closed enums, hex SHAs and integers only. The CLI and MCP render from types, never from the raw header. | None known. |
| T7 | **A3, the honest part.** | On shared-uid wheeld an agent can already overwrite `wheeld`, edit the checkout, or write `state.json`, and could forge an operator request that way. None of these controls hold against A3, and they do not claim to. They hold against A1, A2, A5, A6 and a misbehaving A3. They become real boundaries when per-node uids land (§2, M2/M3) and the data dir, checkout and bin dir are not writable by agent uids. v1 refuses to start updating if `WHEEL_UPDATE_REPO` or `BIN_DIR` is group- or world-writable. | Documented, not closed. Same status as 037's "the gate is the uid, not the path". |

## Rulings requested

- **R1**: default `off`; `prompt` recommended for the self-developing board; `auto` only with branch
  protection (R6).
- **R2**: a range touching `ci` is operator-only.
- **R3**: `wheel update` with zero positional args is the runtime update; §3e's `wheel update <agent>`
  keeps its grammar.
- **R4**: no token means blocked and loud, not an unauthenticated best-effort call. Public repos would
  answer one, but a gate that works until a rate limit is a gate that fails open at random.
- **R5**: requests only from agent nodes.
- **R6**: required reviews plus required checks on `main` before any board runs `auto`.
- **R7**: accept T7 as documented residual until per-node uids.
- **R8**: which shape the operator's box should take to self-update — systemd (`install.sh --updatable`,
  works today, no new code) or compose plus a bin volume (an `infra/` + `docker/` change this lane has not
  made). v1 works on the first as shipped, and refuses loudly on the second.

## v1 implements vs defers

**Implements** (all tested, each gate mutation-checked):
- the `source` driver: fetch, fast-forward-only inspection, `git archive` build off the data volume with a
  private `CARGO_TARGET_DIR`, smoke test that proves the built SHA, atomic swap keeping `.prev`, rollback,
  and a fast-forward of the checkout once the new build is healthy
- policy, path→component mapping and pertinence, and the CI gate (GitHub Actions check-runs only)
- the notice on every `/v1/cli/*` answer, in the CLI (one stderr line, never stdout; `--json` field) and in
  the MCP bridge (once per session per target), plus an MCP `update` tool
- `wheel update`, `POST`/`GET /v1/cli/update`, and `wheeld update [--status]`
- `pause_turns`/`resume_turns`/`busy_agents` on #63's `closing`, and boot resume of parked agents with
  queued work
- the apply pipeline, the rollback watchdog on a plain OS thread, the circuit breaker, `system` messages to
  the requester, and history rows

**Defers**: the Railway and Docker drivers; wheel-web; multi-tenant wheel-host; an operator HTTP route
(the hand path is `wheeld update`); and the compose-mode infra change in R8.
