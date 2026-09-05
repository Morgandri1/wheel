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
| SDK/Engine        | Engine + SDK dev (Rust) | crates/wheel-core, crates/wheel-engine, crates/wheel-host, crates/wheel-cli, docker/, docs/PROTOCOL.md |
| API               | API dev (Rust)          | crates/wheel-api, docs/API.md, infra/ (compose, Railway config, deploy) |
| Web               | Web dev (TypeScript)    | web/ (Next.js, Vercel config), web/src/lib/schema (generated types)  |
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
  crates/wheel-engine/       # ONE PROCESS PER PROJECT (inside its sandbox). sqlite, wire enforcement, agent supervisor, ingress, control plane.
  crates/wheel-host/         # runs on the single big engine machine. Sandbox supervisor: starts/stops per-project engines, routes API→engine.
  crates/wheel-cli/          # `wheel` binary that agents/scripts call inside the sandbox (like `yoke`).
  crates/wheel-api/          # api.wheel.dev — stateless gateway: auth, projects (Postgres), proxy to wheel-host.
  web/                       # wheel.dev — Next.js landing + /app board UI (deployed on Vercel).
  docker/                    # Dockerfile.host (host+engine+cli+claude+codex+python+node) and Dockerfile.api.
  infra/                     # docker-compose for local dev (postgres + api + host), railway.toml per service, deploy notes.
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
- **One sandbox per project, one `wheel-engine` process per sandbox.** Sandboxes are created by `wheel-host` through a
  `Sandbox` trait with two backends: `docker` (local dev / any VM with a docker daemon: container `wheel-p-<id>`, volume `wheel-p-<id>-data`)
  and `process` (production on Railway, where no docker daemon exists: a dedicated unix uid per project, data dir `/data/projects/<id>` mode 0700,
  rlimits, engine control plane on a unix socket `/run/wheel/<id>/engine.sock` owned by that uid — never a TCP port, so tenants cannot reach each other's engines).
  Both backends expose the same engine control plane to the host; nothing above the host knows which backend is in use.
- **Ingress (spec's "Fastify" capability)** is implemented in Rust inside the engine, exposed publicly at
  `https://api.wheel.dev/p/<project_id>/<path>` only while the project capability `http` is enabled (API → host → engine).

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
| agent → ctx          | `wheel read <ctx>` returns the markdown     | `wheel write <ctx> "…"\|--file f.md`  | —                                                          |
| ctx → agent          | —                                           | —                                      | **INJECTION**: ctx markdown is prepended to the agent's prompt on start and after every context clear |
| agent → table        | `wheel read <t>/<row>`, `wheel ls <t>`, `wheel query <t> "<SELECT…>"` (read-only SQL) | + `wheel write <t>/<row> '<json>'` upsert, `wheel rm <t>/<row>` (`write` implies `read`) | — |
| agent → vault        | keys exported as env vars at spawn + `wheel secret get <vault>/<key>` | —              | —                                                          |
| agent → chest        | `wheel read <chest>/<path>` (`--out f`), `wheel ls <chest> [prefix]` | + `wheel write <chest>/<path> --file f`, `wheel rm <chest>/<path>` (`write` implies `read`) | — |
| agent → script       | `wheel run <script> [args…]` (stdout/stderr/exit code returned) | —                   | —                                                          |
| agent → mcp          | MCP server is attached to the agent's harness config at next start | —           | —                                                          |
| endpoint → agent     | —                                           | —                                      | each HTTP hit is delivered as a message (method, path, headers subset, body) |
| endpoint → table     | —                                           | JSON body inserted as a row            | —                                                          |
| endpoint → script    | —                                           | —                                      | script invoked with the request; with `response_mode: script` its stdout is the HTTP response body |
| script → agent       | —                                           | —                                      | `wheel msg` from inside the script (script runs with a token scoped to ITS wires) |
| script → table/chest/vault/ctx | same as agent                     | same as agent                          | —                                                          |

ctx, table, vault, chest, mcp have no other outgoing wires. Agents may **only** act on nodes they are wired to; the
engine checks this on every CLI call using the per-process token — a node's wire set is its capability set.


### The `wheel` CLI and agent preamble — **mimic the Yoke pattern exactly** (PM decision; this is what agents already know)

Agents interact with the board the same way agents in a YOKE swarm do: a CLI whose identity is proven by a per-process token, wire-gated
access with **exit code 3** on denial, and *every node is a keyspace*. Grammar (`<node>` is a node name; `<node>/<row>` addresses a row/path inside it):
```
wheel whoami                              identity: name, id, type, position, wires (both directions)
wheel connections                         my wires with plain-language semantics ("you can prompt it", "you can access its data")
wheel msg <agent> "<text>" | --stdin | --file <path>      send a message; SENDER is derived from my token, never passed
wheel read  <node>                        ctx → markdown; table → all rows as JSON (paged); chest → listing
wheel read  <node>/<row>                  table row as JSON; chest blob (raw, or --out <file>)
wheel write <node> "<value>" | --stdin | --file <path>   ctx → replace markdown
wheel write <node>/<row> "<value>" | --stdin | --file    table → upsert row (value = JSON object matching columns); chest → put blob
wheel rm    <node>/<row>                  table row / chest blob (needs write)
wheel ls    <node> [prefix]               table row keys / chest paths
wheel query <table> "<SELECT …>"          read-only SQL escape hatch, scoped to that one table
wheel secret get <vault>/<key>            vault value (also exported as env at spawn)
wheel run   <script> [args…]              invoke a Script node as a tool; stdout returned
wheel ctx clear                           ask the engine to clear my context (ephemeral pattern)
```
Table nodes therefore always have an implicit primary key column `key TEXT` plus the configured `columns`; `wheel write t/<row>` upserts by key.
Every command prints a one-line human result (and `--json` for machine output). Denials read like: `no wire from <me> to <node> (need: write) — exit 3`.

**Agent preamble.** On every start (and after every context clear) the engine composes the child's system prompt as:
1. the node's `system_prompt`;
2. a generated orchestration block, mirroring YOKE's:
   ```
   ## WHEEL board — agent orchestration
   You are "<name>", an agent on a Wheel board (project <project name>).
   To message a connected agent, run:  wheel msg "TARGET" "your message"
   Your identity is proven from your own credentials — you never pass it.
   ## Board memory (durable, wire-gated)
     wheel read <node> · wheel write <node> "<value>" · wheel read/write <table>/<row> · wheel ls <table> · wheel secret get <vault>/<key> · wheel run <script>
   You can only read/write nodes you're wired to — run `wheel connections` to see yours.
   Your wires: → researcher  send   you can prompt it
               → notes      read   you can access its data
               ← inbox      send   it can prompt you
   ```
3. for each ctx→agent (`send`) wire, an injected block: `\n\n# Context: <ctx name>\n<markdown>`.

**Inbound message framing** (delivered as a user turn on stdin) mirrors YOKE's `AgentPrompt` envelope so agents can't be spoofed by body text:
```
<AgentPrompt id="<ulid>" from="<from name>" type="<from type>">
<body, with any literal `</AgentPrompt>` in the body escaped by the engine>
</AgentPrompt>
```
Messages from the UI use `from="user" type="user"`. Ingress hits use `from="<endpoint name>" type="endpoint"` and a JSON body `{method, path, headers, body}`.

### Message delivery contract
- Messages persist in sqlite (`messages`: id, from_node, to_node, body, created_at, delivered_at, acked_at).
- Delivery into a running agent: engine writes a user turn to the child's stdin using the `<AgentPrompt …>` envelope above.
  Stopped agents queue; queue drains on start. Messages are delivered one at a time; the next is written when the harness reports the turn complete.
- `ephemeral_context: true` → when the agent finishes its turn (harness emits result/idle), engine clears context
  (new session), re-applies system prompt + injected ctx nodes, then continues draining the queue. Agents can also
  request this via `wheel ctx clear`.
- Messages from the UI ("chat" with an agent) are `from_node = user`.

## 4. Engine control plane (inside the sandbox; `:7000` in docker mode, unix socket in process mode; bearer `WHEEL_ENGINE_SECRET`) — SDK owns, host+API proxy, Web consumes

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
ANY    /ingress/*                         → endpoint nodes (only reachable via API /p/<project>/ → host → engine)
POST   /v1/cli/*                          → used by the `wheel` binary; bearer = per-node token (WHEEL_TOKEN env)
```
SDK documents exact request/response bodies in `docs/PROTOCOL.md` and exports JSON Schema for `wheel-core` types into
`docs/schema/*.json` (`cargo run -p wheel-core --bin export-schema`). Web generates TS types from that.

## 4b. Host API (`wheel-host`, on the engine machine, private network only, bearer `WHEEL_HOST_SECRET`) — SDK owns, API consumes

```
GET    /host/v1/healthz                                    → { ok, sandbox_backend: "docker"|"process", projects_running: n }
PUT    /host/v1/projects/:id      {engine_secret, vault_key, capabilities}  → create-or-update sandbox record (idempotent)
POST   /host/v1/projects/:id/start | /stop | /restart      → sandbox lifecycle; start blocks until engine /healthz is green (≤30s) or returns 504
DELETE /host/v1/projects/:id                               → stop + destroy sandbox + data
GET    /host/v1/projects/:id                               → { status: "stopped"|"starting"|"running"|"error", last_error?, started_at? }
ANY    /host/v1/projects/:id/engine/*                      → proxy to that project's engine `/v1/*` (adds its engine bearer; WS bridged)
ANY    /host/v1/projects/:id/ingress/*                     → proxy to that project's engine `/ingress/*`
```
The host is the ONLY process that holds engine secrets at runtime (the API stores them encrypted in Postgres and hands them to the host on `PUT`).
The host persists its sandbox table in `/data/host.db` (sqlite) and reconciles running processes/containers on boot (restart projects that were running).

## 5. Public API (api.wheel.dev, stateless, horizontally scaled) — API owns

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
POST   /v1/projects/:id/start | /stop | /restart                     → sandbox lifecycle (→ host)
ANY    /v1/projects/:id/engine/*                                     → authenticated proxy → host /host/v1/projects/:id/engine/* (incl. WS /engine/v1/events)
ANY    /p/:project_id/*                                              → PUBLIC ingress → host /host/v1/projects/:id/ingress/* (403 if capability `http` disabled; rate-limited)
GET    /healthz
```
- `Project`: `{ id, owner_id, name, capabilities: { http: bool }, status: "stopped"|"starting"|"running"|"error", created_at, updated_at }`.
- API state in Postgres: `projects`, `project_secrets` (engine secret, vault master key — encrypted with API master key from env).
  The API never talks to docker and never talks to an engine directly — everything goes through the host. The API must be safe to run as N replicas
  (no in-memory state that matters; per-replica JWKS cache is fine; rate limits may be per-replica in v1, note it in API.md).

## 5b. Deployment topology (production)

| Piece      | Where                     | Notes |
|------------|---------------------------|-------|
| `web`      | **Vercel**                | Next.js. Env: `NEXT_PUBLIC_API_URL=https://api.wheel.dev`, Clerk keys. Domain `wheel.dev`. |
| `wheel-api`| **Railway**, N replicas   | Stateless. Public domain `api.wheel.dev`. Railway Postgres. Reaches the host over Railway private networking at `http://wheel-host.railway.internal:7100`. Built from `docker/Dockerfile.api`. |
| `wheel-host` | **Railway**, exactly 1 replica, the biggest machine available | Runs every project's engine + agents as `process` sandboxes. Railway volume mounted at `/data`. **No public domain.** Built from `docker/Dockerfile.host` (root inside the container so it can `setuid` per project; drops to the project uid for every child). |

Local dev = `infra/docker-compose.yml`: postgres + api + host (host with `SANDBOX_BACKEND=docker` and the docker socket mounted, OR `process` to mirror prod).
Railway config lives in `infra/railway/<service>/railway.toml` (or `railway.json`); one Railway service per piece; the web is not on Railway.
Residual risks to track (ADVERSARY): all tenants share one kernel and one private network (agents could reach `*.railway.internal` — Postgres must be password-protected and the host secret must never be in a sandbox's env); a single host is a single point of failure (v1 accepts this; host reconciles on restart).

## 6. Milestones

- **M0 — Plan (first thing you do).** `docs/plans/<role>.md` + `STATUS:` to PM.
- **M1 — Vertical slice (target: end of day 1).** One project, running locally via docker-compose (api + host + postgres): create project →
  sandbox starts → place an `agent` (claude) + a `ctx` node, wire ctx→agent (send/injection), authenticate the agent,
  start it, send it a message from the UI, watch it reply in the log, `wheel msg` between two agents works.
  SDK: engine + host (docker backend) + cli + image. API: auth + projects + host client + proxy + WS. Web: board with agent & ctx nodes,
  inspector, start/stop, chat, log. QA: `make check`, smoke test of the slice. ADVERSARY: threat model + first probes.
- **M2 — All node types + full wire matrix + ingress + vault/chest/table/script/mcp + ephemeral context + run_on_startup.**
- **M3 — Hardening (red-team findings fixed), `process` sandbox backend on Railway, landing page, deploy (Railway ×2 + Vercel), docs.**

Ship M1 before polishing anything. Prefer working end-to-end over complete-in-isolation.
