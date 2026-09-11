# AgentGrid on Wheel: engine changes (proposal for PM ruling)

**Author:** SDK/Engine. **Status:** R1–R6 accepted by the operator on 2026-09-11, with the amendments
recorded in §6. Nothing below is built except §0.
**Source plan:** "Wheel as AgentGrid's working engine", Phase 0 through Phase 5.

AgentGrid (the Electron canvas) wants Wheel as a pluggable per-worker engine. Locally that means a
`wheel-engine` sidecar per project directory, running under a new **Desktop** profile. Hosted, it means
the same client pointed at `/v1/projects/:id/engine/v1/*`. This document puts the Wheel-side work in
dependency order so it can be ruled on before any of it is built. Per §1 ("anything that needs a ruling
goes in git"), the rulings are recorded in §6.

Line anchors were verified against this branch's head, not copied from the plan. Several of the plan's
anchors had drifted by 100 to 150 lines.

---

## 0. Landed with this proposal (Phase 0)

`GET /v1/engine` returns `EngineInfo {version, build, api_version, harnesses, profiles, features}`
(`crates/wheel-engine/src/api/engine_routes.rs`, `docs/PROTOCOL.md` §2 "Engine discovery").

- `AgentConfig` is `deny_unknown_fields` (`crates/wheel-core/src/node.rs:173-175`), so AgentGrid has to
  feature-detect before sending any field listed below.
- **Every Phase 1 item that adds a field or route also adds an id to `FEATURES`, in the same commit.**
  The test that holds today's ids to real routes and fields holds the new ones too. The ids proposed
  per item are given below.

## 1. Security framing (applies to everything after it)

**The Desktop profile must be impossible on a hosted engine, not merely unused there.**

Desktop runs the harness as the operator: their `HOME`, their Claude login, their checkout. On a
hosted engine that is full compromise of the host. The first plan risk says the same. So the gate is
structural, and needs all five of these (R2):

1. **Launcher environment only.** Nothing on `/v1/*`, the board, an export or `AgentConfig` can select
   it. It is not a config field, so `deny_unknown_fields` refuses it everywhere clients can write.
2. **A boot failure unless the listener is local.** `Config::from_env`
   (`crates/wheel-engine/src/config.rs:107`) exits non-zero if Desktop is requested with anything but
   a `unix://` listener or a `tcp://` address whose *parsed IP* is loopback (127.0.0.0/8 or `::1`).
   Hostnames, `localhost`, `0.0.0.0` and `[::]` are refused, not resolved. `ListenAddr`
   (`crates/wheel-core/src/spawn.rs:75`) already distinguishes `unix://` from `tcp://`.
3. **The hosted launcher cannot pass it.** The host's process backend builds the engine environment
   from `env_clear` plus an explicit list (`crates/wheel-host/src/sandbox/process.rs:109`, `:375`),
   and a test pins that list's exact key set (`:629`). The Desktop variable must never join that list.
   A test asserting its absence belongs in the same commit as item 1.
4. **Refused from outside: a control, not only a signal.** `EngineInfo.profiles` reports `desktop`
   only when it is active, and the API and host proxies refuse to serve an engine that reports it.
5. **Not compiled into the hosted image.** Desktop sits behind a cargo feature the hosted image never
   enables, verified by the image-contents gate (`make image-verify-prod`). On a hosted engine it is
   then absent, not merely refused.

Each of these is mutation-checked (§0b): remove the check, and the test goes red.

**Credential-path changes need ADVERSARY review before merge**, under the credential-distribution rule
(ARCHITECTURE §3, "Credential-distribution rule"), recorded in `redteam/reviews/`:
- item 1, the profile gate itself, because every other item's safety depends on it (R5);
- item 2 changes which credentials a child sees;
- item 3 lets a child write outside `ws/`;
- item 8 puts rotating tokens into a child's MCP config.

---

## 2. Phase 1: Wheel changes in dependency order

**Item 6 goes first, outside the chain (R1 amendment).** The delivery defects violate today's
contract. They are not AgentGrid prerequisites, so they are fixed before item 1 and depend on nothing.

### 1. `Config.profile = Sandboxed | Desktop`

The profile is set only by the launcher environment, and gated exactly as §1 describes.

- **Rationale:** everything after this depends on a boolean the engine can trust. Keeping it on
  `Config`, and not in per-agent config, is the same reasoning the harness-auth policy uses
  (`config.rs:21-47`, the `harness_auth` field): a knob a project owner can reach is not a policy.
- **Anchors:** `crates/wheel-engine/src/config.rs:21-47` (`Config`), `:107` (`from_env`).
- **Discovery:** `profiles` gains `desktop` when active.

### 2. Desktop spawn uses the operator's login and HOME

- Pass through the launcher-supplied env allowlist.
- Skip remote sanitising, the `CARGO_TARGET_DIR`/`TMPDIR` redirects and `GIT_ASKPASS`.
- `--permission-mode` follows AgentGrid's permission setting instead of the hardcoded
  `bypassPermissions`.

**Rationale:** AgentGrid's restore, `list_recoverable_sessions` and local-runtime fallback all read
`~/.claude/projects`. A session created under an isolated `CLAUDE_CONFIG_DIR` is invisible to them. The
sandbox redirects exist to protect a shared volume that Desktop does not have.

**Anchors:**
- `crates/wheel-engine/src/supervisor/mod.rs:595` (`sanitise_remotes`), `:695` (the one
  `child_command` spawn site), `:727` (`GIT_ASKPASS`), `:770-771` (`CARGO_TARGET_DIR`, `TMPDIR`);
- `crates/wheel-engine/src/harness/claude.rs:38-39` (`bypassPermissions`), `:60-73` (`HOME` and
  `CLAUDE_CONFIG_DIR` pinned to the per-node config dir, `IS_SANDBOX`).

**ADVERSARY review required.** Every exit from `child_command`'s `env_clear` stays inside
`child_command` (`supervisor/mod.rs:232`). The test at `:2852` that forbids any other spawn path stays
green.

### 3. `Workspace::Host { path }`, Desktop only

- **Rationale:** a worker operates in the checkout the user already has. Today every workspace is
  relative and materialised under the engine's root.
- **Anchors:** `crates/wheel-core/src/node.rs:215-221` (`Workspace.path`, "Relative, no `..`"),
  `crates/wheel-engine/src/supervisor/workspace.rs:189-198` (`materialise_one` joins onto `ws_root`).
- **Security:** a Sandboxed engine refuses the variant at node creation *and* at start. The start check
  is the one that holds for a board restored from an export. That is the same two-place pattern as the
  harness refusal (`api/board_routes.rs:53-64`, `supervisor/mod.rs:545`).
- **Discovery:** `host_workspace`.

### 4. New `AgentConfig` launch options

- The fields: `effort`, `permission_mode`, `allowed_tools`/`disallowed_tools`, per-query `max_turns`,
  `system_prompt_mode` (append|replace), opt-outs for the preamble, board MCP and envelope, and
  `external_ref` (AgentGrid's pane id).
- **Rationale:** the engine owns the process, but AgentGrid owns what a worker is. Without these, a
  worker launched through Wheel is a different worker from the same one launched locally, and the
  plan's A/B transcript-parity check cannot pass.
- **Anchors:** `crates/wheel-core/src/node.rs:175-203` (`AgentConfig`),
  `crates/wheel-engine/src/supervisor/prompt.rs:19` (`compose_prompt`),
  `crates/wheel-engine/src/harness/claude.rs:27-58` (`argv`).
- **Envelope opt-out:** the `<AgentPrompt>` envelope is the only thing that attributes a message
  (§3c#5). Opting out has to mean the engine *delivers* only operator-authored turns to that agent
  (`from=user`). Otherwise an agent→agent or endpoint body reaches the child unframed. **Ruled (R4):
  enforced in code, not docs.** A non-user `send` wire into an opted-out agent is refused, setting the
  opt-out while such a wire exists is refused, and delivery re-checks as a backstop.
- **Discovery:** `agent_launch_options`.

### 5. `LogStream::Harness`: raw, redacted harness lines tagged with `message_id`

- Add `--include-partial-messages` and `--replay-user-messages`.
- Partial deltas go over the WebSocket only.
- Add log retention.

**Rationale:** AgentGrid keeps its TypeScript normalizers, so the engine forwards frames and parses
only control events.

**Anchors:**
- `crates/wheel-core/src/event.rs:20-31` (`LogStream`);
- `crates/wheel-engine/src/supervisor/mod.rs:1329`: only an assistant `Text` event is logged, as
  `stdout`. Raw lines reach the log only as `Unknown` (`:1499`).
- `crates/wheel-engine/src/events.rs:19`: a 1024-event broadcast buffer, so deltas must not also be
  persisted.
- `supervisor/mod.rs:1927` inserts into `logs`, and nothing in `crates/wheel-engine/src` ever deletes
  from it.

**Discovery:** `harness_stream`.

### 6. Delivery defects, fixed before the sidecar depends on them

Each one was confirmed by reading the code path, not taken from the plan:

- **`init` sets Idle mid-first-turn.**
  - `supervisor/mod.rs:1298-1309` sets `Idle` on every `Init` unconditionally.
  - `pump_queue` (`:1192`) can write a message as soon as the slot exists, before `init`.
  - So an agent whose first message was enqueued during startup reads `idle` while that turn runs.
  - Fix: settle to `Running` when `in_flight` is set.
- **`mark_error` publishes no message event.**
  - `supervisor/mod.rs:1420-1425`. Both sibling branches publish: the requeue at `:1418` and
    `Consumed` at `:1428`.
  - So a poison message reaches `consumed(error)` without the UI ever seeing it.
- **No message event on enqueue from the control plane or ingress.**
  - `api/agent_routes.rs:159` (`send`) and `api/ingress.rs:360` enqueue without publishing.
  - `api/cli_routes.rs:568` does publish.
  - So agent→agent messages appear as `queued` on `/v1/events`, but user→agent and endpoint→agent
    messages do not.
  - Fix: publish in one place for every enqueue, so a fourth path cannot miss it.
- **Stop mid-turn strands the message in `Delivered`.**
  - `supervisor/mod.rs:1167-1184` takes the slot and kills the child without requeueing.
  - The reaper's run-id guard (`:1538`) then returns early because the slot is empty, so
    `requeue_all_undelivered` (`:1598`) never runs.
  - `/healthz` reports the agent as wedged (`api/mod.rs`, `stalled_agents`), but nothing clears it.
  - Fix: `stop` settles the in-flight message the way `reap` does.
- **Send to a Stopped agent does not start it.** This is *not* a bug. `deliver`
  (`supervisor/mod.rs:1778-1791`) starts only a `Parked` agent. The governing clause is ARCHITECTURE
  §3, "Message delivery contract": "Stopped agents queue; queue drains on start."
  - §3c#14 (idle parking) is the one exception, and it covers `Parked`, the engine's own off, not
    `Stopped`.
  - §3c#13's "a message never starts a process" is about one process per agent, and §3c#14 already
    overrides it for parked agents. It is not the deciding clause.

  The deciding argument: `Stopped` is the operator's off switch. If a send restarted a stopped agent,
  any node holding a `send` wire to it, including a public ingress endpoint, could revive an agent the
  operator deliberately stopped. **Ruled (R3):** keep the contract. AgentGrid calls `start`, checks the
  returned status (`error`, `needs_auth`, `budget_exhausted`), and only then calls `send`.

### 7. `POST /v1/agents/:id/interrupt` and `send {mode:"steer"}`

- `send {mode:"steer"}` allows more than one message in flight. The Claude driver owns the stdin
  writer.
- **Rationale:** AgentGrid's interrupt maps to an engine operation. A signal would kill the session.
- **Anchors:**
  - The interrupt row in PROTOCOL §2 ("Agents") lists it as M2, and it is not routed (`api/mod.rs`,
    `router`).
  - `supervisor/mod.rs:129-148`: `Running` holds the `ChildStdin`, and `in_flight` holds one message.
  - `pump_queue` (`:1192`) is the only stdin writer (§3c#12). Steering must stay inside it, or the
    single-writer rule is gone.
- **Discovery:** `interrupt`, `steer`.

### 8. Runtime MCP overlay, `PUT /v1/agents/:id/runtime`

- Never persisted, and merged into the MCP config on every start.
- **Rationale:** AgentGrid's canvas and terminal MCP servers carry a port and token that rotate on
  every launch. As board data they would leak through `/v1/board`, exports and templates.
- **Anchors:** `supervisor/mod.rs:210-230`: `write_mcp_config` writes only the built-in `wheel`
  server, called at `:680`.
- **ADVERSARY review required:** a token in a child's MCP config is a credential handed to a child.
- The test that proves the overlay never appears in `/v1/board` is the gate.
- **Discovery:** `runtime_mcp`.

### 9. Image attachments on send

- **Rationale:** desktop workers already send multimodal turns.
- **Anchors:** `api/agent_routes.rs:26-30` (`SendBody.body` is a `String`),
  `crates/wheel-core/src/message.rs:24` (`MAX_MESSAGE_BODY`, 256 KiB).
- The sha256/bytes receipt (§3c#3) has to cover attachments too, or a mangled image is undetectable.
- **Discovery:** `image_attachments`.

### 10. Graceful shutdown stops every agent

- **Not proposed here.** The API lane is implementing it on branch `api/headless-first`, as part of
  headless-first `wheeld`.
- Today `crates/wheel-engine/src/lib.rs:98-149` serves with `with_graceful_shutdown` and stops no
  agents.
- AgentGrid's sidecar depends on that branch landing. Nothing here duplicates it.

---

## 3. Phase 2: one driver contract, then every harness

Replace `Harness` with `HarnessDriver::launch() → DriverSession`:
- **Methods:** `send_turn`, `steer`, `interrupt`, `shutdown`.
- **Events:** `Ready`, `SessionStarted`, `Frame`,
  `TurnComplete{usage,cost,is_error,interrupted}`, `NeedsAuth`, `RateLimited`, `Exited`.

**Rationale:** Codex approvals and ACP permission requests need stateful replies on stdin. The current
trait is a stateless line parser plus argv (`crates/wheel-engine/src/harness/mod.rs:101-128`) and
cannot express that.

Where Claude is baked in:
- `supervisor/mod.rs:129-148`: `Running` owns the stdin.
- `:799`: `start` spawns eagerly.
- `:1524`: `reap` treats stdout EOF as death.
- `:279`: `ClaudeDriver` is hardcoded.
- `crates/wheel-core/src/node.rs:150`: the `Harness` enum.

A registry replaces the hardcoding:
- `has_driver` (`harness/mod.rs:20`, landed in §0) becomes a registry lookup, so node creation, start
  and `EngineInfo.harnesses` keep giving one answer.
- `child_command` stays the only spawn path, enforced by `supervisor/mod.rs:2852`.

Port order, lowest risk first: Claude (re-hosted) → the per-turn resume CLIs → Codex app-server →
ACP/Devin → OpenCode → Antigravity.

Phase 2 comes after Phase 1 because the refactor touches the forged-result defence (F008), reaping and
parking. Those are Wheel's hardest-won guarantees and should move behind a green Phase 1 suite, not
alongside it.

## 4. Phase 5: hosted gating

- **Transport:** hosted AgentGrid uses a Better Auth JWKS token (`AUTH_MODE=jwks`), requests to
  `/v1/projects/:id/engine/v1/*`, and a WebSocket ticket.
- **Routing:** the API's wildcard (`crates/wheel-api/src/lib.rs:107`) already carries
  `/engine/v1/engine` to the engine. The dedicated events route (`:101`) matches only
  `/engine/v1/events`, so the two do not collide. The host proxies the same way
  (`crates/wheel-host/src/lib.rs:587`). A test in `crates/wheel-api/tests/routes_db.rs` pins the API hop.
- **Profile and credentials:** hosted agents stay Sandboxed, with API-key-only harness auth
  (`WHEEL_HARNESS_AUTH`, `docs/proposals/wheel-harness-auth.md`) and git workspaces.
- **Gated on** per-node uids (`redteam/findings/037-single-uid-blast-radius.md`) and network isolation
  (`redteam/findings/048-s5b-network-isolation-not-deployed.md`). This is a security gate, not rollout
  polish.

## 5. Out of scope

- Script and chest nodes.
- MCP-node attachment (M2).
- The Windows port (plan Phase 3).
- Canvas node panes (plan Phase 4).
- Licensing. A written commercial grant is needed before any binary is bundled, and that is not an
  engineering ruling.

## 6. Rulings (operator, 2026-09-11)

All six were accepted as recommended. The reviewer's amendments were adopted with them.

- **R1: approved, amended.** The Phase 1 order in §2 stands, with item 10 owned by
  `api/headless-first`. Item 6 (the delivery defects) moves out of the dependency chain and is fixed
  first: those are violations of today's contract, not AgentGrid prerequisites.
- **R2: approved, as a boot failure, not a warning. Tightened three ways (§1):**
  - "loopback" means the parsed IP (127.0.0.0/8, `::1`), and hostnames, `localhost`, `0.0.0.0` and
    `[::]` are refused;
  - check 4 is a control: the API and host proxies refuse to serve an engine reporting `desktop`.
    Every hosted request passes through the host proxy; the API check is the second layer;
  - Desktop sits behind a cargo feature the hosted image never enables, verified by the image-contents
    gate, so there it is impossible rather than refused.
- **R3: keep the contract.** The governing clause is ARCHITECTURE §3 ("Stopped agents queue; queue
  drains on start"), with §3c#14 as the parked exception. AgentGrid calls `start`, checks the returned
  status (`error`, `needs_auth`, `budget_exhausted`), and only then calls `send`.
- **R4: agreed, enforced in code:**
  - a non-user `send` wire into an agent with the envelope opt-out is refused;
  - setting the opt-out while such a wire exists is refused;
  - delivery re-checks as a backstop.
- **R5: agreed.** ADVERSARY reviews items 1, 2, 3 and 8 before merge. Item 1 is on the list because
  every other item's safety depends on the profile gate.
- **R6: agreed.** `GET /v1/engine` and its additive-only rule are now in ARCHITECTURE §4, in the same
  PR as this proposal.

None of R2's or R4's controls is built by this PR.
