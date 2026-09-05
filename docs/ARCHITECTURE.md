# WHEEL — Shared Project Contract (v1, authored by PM)

You are one agent on a five-agent team building **Wheel**: a per-project, per-user Docker container that runs
continuously in the cloud with Claude Code / Codex instances as child processes. Agents talk to each other over
"wires" and access other nodes on a visual board. This document is the single source of truth for cross-team
contracts. If you need to change anything here, message PM first (`yoke msg PM "PROPOSAL: ..."`). Do not
silently diverge.

## 0. Team, ownership, and how we communicate

| Agent (yoke name) | Role                    | Owns (paths in the monorepo)                                         |
|-------------------|-------------------------|----------------------------------------------------------------------|
| PM                | Project manager (me)    | docs/ARCHITECTURE.md, docs/plans/, final decisions                   |
| SDK/Engine        | Engine + SDK dev (Rust) | crates/wheel-core, crates/wheel-engine, crates/wheel-cli, docker/, docs/PROTOCOL.md |
| API               | API dev (Rust)          | crates/wheel-api, docs/API.md, infra/ (compose, deploy)              |
| Web               | Web dev (TypeScript)    | web/ (Next.js), web/src/lib/schema (generated types)                 |
| QA                | QA engineer             | qa/, Makefile `check` targets, CI config, docs/TESTPLAN.md           |
| ADVERSARY         | Red team                | redteam/ (threat model, findings, PoCs)                              |

**Messaging.** Run `yoke connections` to see who you can reach. Everyone can reach PM. Route cross-team questions
through PM unless you have a direct wire. Message format (one per message, first line is the tag):
- `STATUS: <what you finished / what's next>` — send at every meaningful milestone (at least every ~hour of work).
- `BLOCKED: <what> — <what you need> — <what you're doing meanwhile>` — never sit idle; pick up unblocked work.
- `QUESTION: <specific question + your recommended answer>` — always include your recommendation.
- `DONE: <deliverable>` — with the commit hash and how to verify.
- `BUG: <title> | severity | repro steps | expected vs actual` (QA/ADVERSARY → owner via PM).
- `PROPOSAL: <change to shared contract>` — PM will accept/reject.

**Working rhythm.** (1) Read this contract and your role brief in full. (2) Write a plan to `docs/plans/<role>.md`
(milestones, file layout, open questions, risks) and send PM a `STATUS:` summarising it. (3) Execute the plan.
Do not wait for PM to approve the plan unless you have a blocking `QUESTION:` — you have authority within your
ownership area. Ship small, commit often, keep main green.

## 1. Repository & workflow

- Monorepo at `/Users/metatron/wheel` (git, branch `main`). PM has seeded it. Never rewrite history on `main`.
- Each agent works in its own **git worktree** so we don't fight over one index:
  `git -C /Users/metatron/wheel worktree add /Users/metatron/wheel-wt/<role> -b <role>/main` (role = sdk | api | web | qa | redteam).
- Integrate frequently: rebase your branch on `main`, run `make check` (QA owns it; until it exists run your own
  crate/package tests), then `git -C /Users/metatron/wheel merge --no-ff <role>/main`. If the merge lock is held, retry.
- Only touch paths you own. If you must edit another team's path, message the owner (via PM) with the diff.
- Commit messages: `<area>: <imperative summary>` e.g. `engine: enforce wire matrix on cli calls`.
- Toolchain (host is macOS, Docker present, **no cargo/node installed yet**): install Rust via
  `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y` (stable), Node 22 + pnpm via
  `brew install node pnpm` (or fnm). Python 3 is present. Docker Desktop/OrbStack is running.

Layout:
```
wheel/
  Cargo.toml                 # workspace: wheel-core, wheel-engine, wheel-cli, wheel-api
  crates/wheel-core/         # shared types (serde) — Node, Wire, NodeType, configs, events, wire matrix. SOURCE OF TRUTH.
  crates/wheel-engine/       # runs INSIDE the project container. sqlite, wire enforcement, agent supervisor, ingress, control plane.
  crates/wheel-cli/          # `wheel` binary that agents/scripts call inside the container (like `yoke`).
  crates/wheel-api/          # api.wheel.dev — auth, projects, container orchestration, proxy to engine.
  web/                       # wheel.dev — Next.js landing + /app board UI.
  docker/                    # Dockerfile for the project container (engine + cli + claude + codex + python + node).
  infra/                     # docker-compose for local dev (api + postgres), deploy notes.
  docs/                      # ARCHITECTURE.md (this), PROTOCOL.md, API.md, TESTPLAN.md, plans/, schema/ (JSON schema export)
  qa/                        # integration + e2e tests, fixtures.
  redteam/                   # threat model, findings, PoCs.
  Makefile                   # `make check` (QA), `make dev` (API), `make engine-image` (SDK)
```

## 2. Tech decisions (final unless PM changes them)

- **Engine, CLI, API: Rust** (stable, edition 2021). HTTP: `axum` + `tokio`. sqlite: `rusqlite` (bundled). Docker: `bollard`.
  Postgres (API only): `sqlx`. Errors: `thiserror`/`anyhow`. Serialization: `serde` + `serde_json`. IDs: `uuid` v4. Time: RFC3339 UTC.
- **Web: Next.js 15 (App Router) + TypeScript + Tailwind + `@xyflow/react`** (board canvas) + TanStack Query + Clerk React.
- **Auth: Clerk** (hosted; email/password + Google/GitHub/SAML SSO). Web obtains a Clerk session JWT; API verifies it
  against Clerk JWKS (`RS256`), using `sub` as the user id. No home-grown password storage.
- **Agents run as child processes** of the engine: `claude` CLI and `codex` CLI binaries baked into the container
  image. No Node SDKs in the Rust engine — we drive the CLIs' stream-JSON / JSONL protocols over stdin/stdout.
- **Storage inside the container**: one sqlite file `/data/wheel.db` (nodes, wires, messages, runtime state, Table-node
  data, vault ciphertext, chest index). Chest blobs on disk `/data/chest/<node_id>/`. Scripts on disk `/data/scripts/<node_id>/`.
- **One container per project**, image `wheel-engine:<tag>`, container name `wheel-p-<project_id>`, volume `wheel-p-<project_id>-data` mounted at `/data`.
  Ports are not published; the API reaches the engine on the docker network at `http://wheel-p-<id>:7000`.
- **Ingress (spec's "Fastify" capability)** is implemented in Rust inside the engine, exposed publicly at
  `https://api.wheel.dev/p/<project_id>/<path>` only while the project capability `http` is enabled.

## 3. Canonical data model (crates/wheel-core — SDK owns, everyone consumes)

All nodes share these traits (per spec): `name`, `position`, `wires`, `type`. Canonical JSON:

```jsonc
{
  "id": "uuid",
  "name": "researcher",            // unique per project; ^[a-z0-9][a-z0-9-_]{0,62}$ ; this is the address agents use
  "type": "agent",                 // agent | ctx | table | endpoint | script | mcp | vault | chest
  "position": { "x": 120.0, "y": 340.0 },
  "wires": [ { "to": "<node id>", "type": "read" } ],   // OUTGOING wires only; type: read | write | send
  "config": { ... }                // tagged by `type`, see below
}
```
Runtime state (NOT stored in config; reported alongside as `state`): agents → `status` (`stopped | starting | needs_auth | running | idle | error`), `session_id`, `last_activity`, `last_error`.

Per-type `config`:
- `agent`:    `{ harness: "claude" | "codex", model?: string, system_prompt: string, run_on_startup: bool, ephemeral_context: bool }`
- `ctx`:      `{ markdown: string }`
- `table`:    `{ columns: [{ name: string, type: "text"|"integer"|"real"|"blob"|"json" }] }` → sqlite table `t_<node.name>` (engine renames on node rename)
- `endpoint`: `{ method: "GET"|"POST"|"PUT"|"DELETE", path: string /* leading slash, no `..` */, response_mode: "ack" | "script" }`
- `script`:   `{ language: "python" | "ts" | "js", source: string, timeout_secs?: number /* default 60 */ }`
- `mcp`:      `{ transport: "stdio" | "http", command?: string, args?: string[], url?: string, env?: {k: v} }`
- `vault`:    `{ keys: string[] }` — values are WRITE-ONLY through the API (`PUT /vault/<node>/<key>`), never returned to the UI; stored encrypted at rest with a per-project key.
- `chest`:    `{}` — blob store; keys are relative paths, no `..`, no absolute paths, max 50 MiB per blob (v1).

### Wire semantics matrix (default DENY — anything not listed is rejected at creation time by engine AND api)

| from → to            | read                                        | write                                  | send                                                       |
|----------------------|---------------------------------------------|----------------------------------------|------------------------------------------------------------|
| agent → agent        | —                                           | —                                      | `wheel msg <agent> "..."` delivers into the target's inbox |
| agent → ctx          | `wheel read <ctx>` returns the markdown     | `wheel write <ctx> --file f.md`        | —                                                          |
| ctx → agent          | —                                           | —                                      | **INJECTION**: ctx markdown is prepended to the agent's prompt on start and after every context clear |
| agent → table        | `wheel table query <t> "<SELECT…>"` (read-only) | + INSERT/UPDATE/DELETE (`write` implies `read`) | —                                              |
| agent → vault        | keys exported as env vars at spawn + `wheel secret get <vault>/<key>` | —              | —                                                          |
| agent → chest        | `wheel chest get|ls <chest> <key>`          | + `wheel chest put|rm` (`write` implies `read`) | —                                             |
| agent → script       | `wheel run <script> [args…]` (stdout/stderr/exit code returned) | —                   | —                                                          |
| agent → mcp          | MCP server is attached to the agent's harness config at next start | —           | —                                                          |
| endpoint → agent     | —                                           | —                                      | each HTTP hit is delivered as a message (method, path, headers subset, body) |
| endpoint → table     | —                                           | JSON body inserted as a row            | —                                                          |
| endpoint → script    | —                                           | —                                      | script invoked with the request; with `response_mode: script` its stdout is the HTTP response body |
| script → agent       | —                                           | —                                      | `wheel msg` from inside the script (script runs with a token scoped to ITS wires) |
| script → table/chest/vault/ctx | same as agent                     | same as agent                          | —                                                          |

ctx, table, vault, chest, mcp have no other outgoing wires. Agents may **only** act on nodes they are wired to; the
engine checks this on every CLI call using the per-process token — a node's wire set is its capability set.

### Message delivery contract
- Messages persist in sqlite (`messages`: id, from_node, to_node, body, created_at, delivered_at, acked_at).
- Delivery into a running agent: engine writes a user turn to the child's stdin framed as
  `[wheel] message from <from_name> (<from_type>):\n<body>`. Stopped agents queue; queue drains on start.
- `ephemeral_context: true` → when the agent finishes its turn (harness emits result/idle), engine clears context
  (new session), re-applies system prompt + injected ctx nodes, then continues draining the queue. Agents can also
  request this via `wheel ctx clear`.
- Messages from the UI ("chat" with an agent) are `from_node = user`.

## 4. Engine control plane (inside container, `:7000`, bearer `WHEEL_ENGINE_SECRET`) — SDK owns, API proxies, Web consumes

```
GET    /v1/board                          → { nodes: [Node+state], project: {...} }
POST   /v1/nodes                          → create (validates name, type, config)
PATCH  /v1/nodes/:id                      → name/position/config (partial)
DELETE /v1/nodes/:id                      → cascades wires; drops t_ table / chest dir
POST   /v1/wires      {from,to,type}      → validated against the matrix
DELETE /v1/wires      {from,to,type}
POST   /v1/agents/:id/start|stop|restart|clear
POST   /v1/agents/:id/send  {body}        → user → agent message
GET    /v1/agents/:id/log?since=<cursor>  → JSON lines (also streamed on /v1/events)
POST   /v1/agents/:id/auth/begin          → { mode: "device_code"|"paste_code"|"api_key", url?, user_code?, instructions }
POST   /v1/agents/:id/auth/complete {code?|api_key?}
GET    /v1/agents/:id/auth                → { authenticated: bool, account?: string }
PUT    /v1/vault/:id/:key   {value}       → write-only
GET    /v1/tables/:id/rows?limit&offset   → for the UI table viewer;  POST /v1/tables/:id/query {sql} (read-only)
GET    /v1/chests/:id/ls?prefix ; GET /v1/chests/:id/blob?key ; PUT /v1/chests/:id/blob?key (raw body)
GET    /v1/events   (WebSocket)           → { type: "node.state"|"message"|"log"|"board.changed", ... }
ANY    /ingress/*                         → endpoint nodes (only reachable via the API's /p/<project>/ route)
POST   /v1/cli/*                          → used by the `wheel` binary; bearer = per-node token (WHEEL_TOKEN env)
```
SDK documents exact request/response bodies in `docs/PROTOCOL.md` and exports JSON Schema for `wheel-core` types into
`docs/schema/*.json` (`cargo run -p wheel-core --bin export-schema`). Web generates TS types from that.

## 5. Public API (api.wheel.dev) — API owns

- Every project-scoped request carries `x-auth-token: <Clerk session JWT>` and `x-project-id: <uuid>`.
  Order of operations, always: verify JWT → load project by id → **assert `project.owner_id == jwt.sub`** → then anything else.
  Non-owned / non-existent projects return **404** (no enumeration). Missing/invalid token → 401.
- Routes:
```
POST   /v1/projects                     {name}                       → Project
GET    /v1/projects                                                  → [Project]   (x-project-id not required)
GET    /v1/projects/:id                                              → Project + container status
PATCH  /v1/projects/:id                 {name?, capabilities?: {http: bool}}
DELETE /v1/projects/:id                                              → stops + removes container + volume
POST   /v1/projects/:id/start | /stop | /restart                     → container lifecycle
ANY    /v1/projects/:id/engine/*                                     → authenticated proxy to the container's control plane (incl. WS /engine/v1/events)
ANY    /p/:project_id/*                                              → PUBLIC ingress → container /ingress/* (403 if capability `http` disabled; rate-limited)
GET    /healthz
```
- `Project`: `{ id, owner_id, name, capabilities: { http: bool }, status: "stopped"|"starting"|"running"|"error", created_at, updated_at }`.
- API state in Postgres: `projects`, `project_secrets` (engine secret, vault master key — encrypted with API master key from env).

## 6. Milestones

- **M0 — Plan (first thing you do).** `docs/plans/<role>.md` + `STATUS:` to PM.
- **M1 — Vertical slice (target: end of day 1).** One project, running locally via docker-compose: create project →
  container starts → place an `agent` (claude) + a `ctx` node, wire ctx→agent (send/injection), authenticate the agent,
  start it, send it a message from the UI, watch it reply in the log, `wheel msg` between two agents works.
  SDK: engine + cli + image. API: auth + projects + container start + proxy + WS. Web: board with agent & ctx nodes,
  inspector, start/stop, chat, log. QA: `make check`, smoke test of the slice. ADVERSARY: threat model + first probes.
- **M2 — All node types + full wire matrix + ingress + vault/chest/table/script/mcp + ephemeral context + run_on_startup.**
- **M3 — Hardening (red-team findings fixed), landing page, deploy (`infra/`), docs.**

Ship M1 before polishing anything. Prefer working end-to-end over complete-in-isolation.
