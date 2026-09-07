# Operator directive — dogfood the cloud board, then one massive RAM/disk optimization PR (2026-09-07)

Two phases. Phase 2 is GATED on Phase 1; it does not start until the trigger below is true.

## Phase 1 — dogfood the cloud board (active)

Objective: make the production wheel-dev board (`6906cadb`) genuinely usable as our primary dev
environment — "running fully on its own," operated as a board, not hand-edited.

Ground truth (live `wheel.db`, read-only): 6 agents meshed by 33 `send` wires, 8 `ctx` injections,
a `reports` table, the `telegram` endpoint + `telegram_out` tool, a `secrets` vault; 47 wires. The swarm
already dogfoods `agent/ctx/endpoint/table/tool/vault`. The gaps ARE the product gaps we can't yet exercise:

- **Script execution** — `NodeType::Script` + `ScriptConfig` + `wheel run <script>` are modeled, but the
  execution runtime is unimplemented (the wall ADVERSARY hit on the egress probe). Hypothesised keystone.
  Lands with ADVERSARY's egress PoC as a required acceptance gate + QA ≥90%.
- **MCP / Chest** — modeled (`McpConfig`/`ChestConfig`, preamble lines), not exercised on the board;
  likely share the Script runtime pattern.
- **UI operability (Web + API)** — can a user place a node, draw a wire (default-DENY matrix), toggle an
  agent's run-on-startup / ephemeral-context / system-prompt, read a Table/Chest, edit a Ctx, and see agent
  state (Parked/Running/Idle) in wheel.dev/app — or is the board only configurable by hand-editing the DB?

Completion criterion (the Phase-2 trigger): the board can be BUILT and OPERATED end-to-end from the UI,
all node types (Script/MCP/Chest included) execute, and the swarm runs the full dev flow
(clone → contextualise → ask → edit → commit → push) on the cloud board without external scaffolding.

## Phase 2 — one massive RAM/disk optimization PR (GATED on Phase 1 completion)

When the trigger above is true: instruct the cloud board (its own PM agent) to produce ONE PR that
optimizes RAM and disk usage as hard as humanly possible. It runs on the dogfooded board — the board
optimizing itself is the point.

Seed levers already measured tonight (so it does not start cold):
- pnpm store-dir per PROJECT rather than per agent (~1.76G today across two homes; re-fetchable, shared).
- per-project CARGO_HOME already exists — mirror it for pnpm (`PNPM_HOME`/`store-dir`).
- API binary already flipped `--no-default-features --features postgres` (−28%); apply the same driver-only
  discipline to `wheeld` and the host.
- `wheeld` still pulls `sqlx` on SQLite (~34 crates, known debt, feature-gate it).
- the six orphaned `creds/<uuid>/wheel` checkouts were reclaimed; A8 collapses git objects, not working files.
- measure before/after `df` and RSS; the PR states the numbers, not "should be smaller."
