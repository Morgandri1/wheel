# WHEEL — Engine protocol (v1)

**Owner:** SDK/Engine. **Consumers:** API (proxies), Web (renders), QA (asserts), ADVERSARY (attacks).
Authoritative for everything between the engine and the world. Where this doc and `ARCHITECTURE.md` disagree,
ARCHITECTURE wins and this is a bug — tell PM.

Types are generated from `crates/wheel-core` into `docs/schema/*.json`. **Those files are normative for JSON
shapes**; this document is normative for routes, semantics, ordering and errors.

---

## 0. Status of this version

Implemented in `wheel-core` and pinned by tests: the data model, the wire matrix, the `<AgentPrompt>` envelope,
the agent preamble, delivery states, limits, the engine spawn contract.
Not yet implemented (documented here so API/Web/QA can build against a fixed target): the routes themselves,
which land with the engine in M1/M2 per the milestone column in each table.

---

## 1. Transport, auth and errors

The engine listens where `WHEEL_LISTEN` says (§7): `tcp://0.0.0.0:7000` in docker mode, `unix:///run/wheel/<id>/engine.sock`
in process mode. It is never exposed publicly — API → host → engine.

Three authentication realms, deliberately disjoint:

| Realm | Header | Credential | Reaches |
|---|---|---|---|
| Control plane | `Authorization: Bearer <WHEEL_ENGINE_SECRET>` | per-project, held only by the host | `/v1/*` except `/v1/cli/*` |
| CLI / agent | `Authorization: Bearer <WHEEL_TOKEN>` | per-node, rotated on every agent start | `/v1/cli/*` only |
| Ingress | none | — | `/ingress/*` only |

**The engine secret is never in a child process's environment.** A node token identifies exactly one node and is
the whole basis of that node's authority: the engine resolves token → node → wire set on every call. Tokens are
32 random bytes, stored in sqlite as a SHA-256 hash, and invalidated when the agent stops.

`GET /healthz` needs no auth and returns `200 {"ok":true}` — the host's readiness probe (§7).

### Error body

Every non-2xx response from every route:

```json
{ "error": { "code": "wire_denied", "message": "no wire from planner to secrets (need: read)" } }
```

| code | HTTP | Meaning |
|---|---|---|
| `unauthorized` | 401 | Missing/invalid bearer for the realm. |
| `wire_denied` | 403 | Caller's node has no wire to the target granting the required type. |
| `not_found` | 404 | No such node/row/blob/message. Also returned instead of 403 where existence itself is sensitive. |
| `name_taken` | 409 | A node with that name already exists in this project. |
| `invalid` | 400 | Failed validation (name charset, config, path, SQL, limits). `message` says which. |
| `too_large` | 413 | Exceeded a §6 limit. |
| `conflict` | 409 | Illegal state transition (e.g. `auth/complete` for a finished session). |
| `harness_error` | 502 | The agent's CLI failed in a way the engine could not recover. |
| `timeout` | 504 | Script/query exceeded its deadline. |

`wire_denied` is also emitted as a `wire.denied` WS event so denials are visible in the UI and to QA/ADVERSARY,
not silent.

---

## 2. Control plane (`/v1/*`, engine-secret bearer)

### Board

| Route | Body → Response | M |
|---|---|---|
| `GET /v1/board` | → `{ nodes: NodeWithState[], project: {id, name, capabilities} }` | M1 |
| `POST /v1/nodes` | `{name, type, config, position}` → `Node` | M1 |
| `PATCH /v1/nodes/:id` | `{name?, position?, config?}` (partial) → `Node` | M1 |
| `DELETE /v1/nodes/:id` | → `204` | M1 |
| `POST /v1/wires` | `WireSpec {from,to,type}` → `204` | M1 |
| `DELETE /v1/wires` | `WireSpec` → `204` | M1 |

`GET /v1/board` is the only board read; it returns each node with its runtime `state` (agents only, for now).
**Vault values are never included** — a vault node returns only its `config.keys`.

Node creation validates, in order: name charset + reserved names (`user`, `wheel`, `system`, `engine`) + uniqueness;
config against its type; then side effects (create `t_<name>` for a table, `chest/<id>/` for a chest).
A rename of a table node renames its sqlite table in the same transaction.

`DELETE /v1/nodes/:id` cascades: removes wires in **both** directions, drops `t_<name>`, deletes the chest
directory and script directory, stops a running agent, and deletes that node's queued messages.

Wire creation is checked against the §3 matrix by `wheel_core::check_wire` — **the same function the API calls**,
so the two cannot disagree. Self-wires are rejected. Creating a wire that already exists is idempotent (`204`).

### Agents

| Route | Body → Response | M |
|---|---|---|
| `POST /v1/agents/:id/start` | → `{status, session_id?}` | M1 |
| `POST /v1/agents/:id/stop` | → `{status}` | M1 |
| `POST /v1/agents/:id/restart` | → `{status, session_id?}` | M1 |
| `POST /v1/agents/:id/clear` | → `{status, session_id}` — new session, prompt re-composed | M1 |
| `POST /v1/agents/:id/send` | `{body, reply_to?}` → `MessageReceipt` | M1 |
| `GET /v1/agents/:id/log?since=<seq>&stream=<s>&limit=<n>` | → `{lines: LogLine[], next: <seq>}` | M1 |
| `POST /v1/agents/:id/interrupt` | → `{status}` — cancels the in-flight turn | M2 |
| `GET /v1/agents/:id/inbox?since=<ts>&limit=<n>` | → `{messages: Message[]}` | M1 |
| `GET /v1/agents/:id/inbox/:message_id` | → `Message` (exact original body) | M1 |

**`start` is idempotent** (§3c#13): starting a running agent is a no-op that returns the existing `session_id`.
The supervisor holds a per-agent mutex across spawn, so concurrent starts cannot race into two processes.
**A message never starts a process** — it enqueues.

`GET .../log` `stream` filter accepts `stdout | stderr | engine | transcript`. `transcript` is the exact bytes
the engine wrote to the child's stdin (§3c#10), exposed on this same route and as ordinary `log` events so the
UI needs no second subscription (agreed with Web, M2). `seq` is monotonic per agent and is the resume cursor.

### Auth (per agent node)

| Route | Body → Response | M |
|---|---|---|
| `POST /v1/agents/:id/auth/begin` | `{mode?}` → `AuthBegin {mode, url?, user_code?, instructions, session}` | M2 |
| `POST /v1/agents/:id/auth/complete` | `{code?}` \| `{api_key?}` → `AuthStatus` | M2 |
| `GET /v1/agents/:id/auth` | → `AuthStatus {authenticated, account?}` | M2 |

Credentials live per node under `<data>/creds/<node_id>/` and each child is spawned with its own config dir, so
two agent nodes in one sandbox can be two different accounts. `auth/complete` for a `device_code` flow is a
**poll** (the engine is already polling; this returns current status), not a submit. Findings from the auth spike
are being folded in; until a path is verified end-to-end the engine reports `needs_auth` rather than pretending.

### Data nodes

| Route | Notes | M |
|---|---|---|
| `PUT /v1/vault/:id/:key` | `{value}` → `204`. Write-only: no route ever returns a vault value to the control plane. | M2 |
| `GET /v1/tables/:id/rows?limit&offset` | → `{rows: object[], total}` | M2 |
| `POST /v1/tables/:id/query` | `{sql}` → `{columns, rows}`. Read-only, 5s timeout. | M2 |
| `GET /v1/chests/:id/ls?prefix` | → `{entries:[{key,bytes,modified_at}]}` | M2 |
| `GET /v1/chests/:id/blob?key` | → raw bytes | M2 |
| `PUT /v1/chests/:id/blob?key` | raw body → `204` | M2 |

Table SQL runs on a **separate read-only sqlite connection** with a `set_authorizer` that makes only that one
`t_<name>` visible; `ATTACH`, `DETACH`, `PRAGMA` and any write verb are rejected before execution.

### Events — `GET /v1/events` (WebSocket)

One JSON object per frame; shapes in `docs/schema/event.json`.

| `type` | Payload | Fires when |
|---|---|---|
| `node.state` | `{node_id, state}` | An agent's status/session/error changes. |
| `message` | `{message}` | A message is created, delivered or consumed (state transition). |
| `log` | `{line}` | A line of stdout/stderr/engine/transcript output. |
| `board.changed` | `{at}` | Nodes or wires changed — client refetches `GET /v1/board`. |
| `wire.denied` | `{denial}` | A capability check failed. |

`board.changed` is deliberately coarse: the board is small, and a second mutation protocol would inevitably drift
from the REST one. Subscribers get a snapshot by calling `GET /v1/board` on connect; there is no replay.

---

## 3. Message delivery

The rules that matter, all from §3/§3c, all testable:

1. **Persist first.** A message is written to sqlite (`queued`) before any delivery is attempted. A crash mid-delivery
   loses nothing.
2. **Single writer** (§3c#12). The per-agent delivery loop is the *only* code that writes to a child's stdin.
   Nothing else — not logs, not scripts, not the MCP server, not `exec` — has a path to it.
3. **Strictly serial.** One message per turn. The next line is written only after the harness emits `result`.
4. **Priority lane.** `from=user` messages are ordered ahead of queued agent/endpoint/script messages, but are
   **never injected mid-turn**. Interrupting is a separate explicit action (`/interrupt`, M2).
5. **One process per agent** (§3c#13). Messages never spawn; they enqueue.
6. **Never truncate** (§3c#11). If a body cannot be delivered the message stays `queued` with `last_error` set and
   is surfaced in the UI. Nothing is ever silently clipped.
7. **Byte-exact** (§3c#3). `sha256` and `bytes` are stored on the row and returned by `wheel msg`. What arrives
   inside the envelope is byte-identical to what was sent, modulo the documented close-tag escape.

### States

`queued → delivered → consumed`, forward only (`MessageState::can_advance_to` rejects everything else, so a bug
cannot walk a message backwards into a re-delivery).

| State | Means |
|---|---|
| `queued` | Persisted, not yet written. Agent stopped, or an earlier message is in flight. |
| `delivered` | Written to the child's stdin. |
| `consumed` | The harness reported the turn containing it complete (`result`). |

### The envelope

Exactly these bytes, as the `text` of a stream-json user turn:

```
<AgentPrompt id="<message uuid v4>" from="<from name>" type="<agent|user|endpoint|script|system>">
<body>
</AgentPrompt>
```

`reply_to="<uuid>"` is added to the open tag when the message is a reply (M2).

Attribution is **engine-generated and unforgeable**: the `from`/`type` attributes come from the resolved sender
node, never from anything the sender controls. A body containing `</AgentPrompt>` (any case) has the `/` escaped
to `<\/AgentPrompt>` so it cannot close the envelope early and open a forged one. `wheel inbox <id>` returns the
original unescaped body from sqlite, so nothing is lost.

---

## 4. Harness spawn contract

### Claude (`harness: "claude"`) — M1

Verified against Claude Code **2.1.261**.

```
claude --print
       --input-format stream-json
       --output-format stream-json
       --verbose
       --permission-mode bypassPermissions
       --append-system-prompt-file <path>
       [--model <model>]        # omitted entirely when config.model is null
       [--mcp-config <path>]    # only when >=1 mcp node is wired
       [--resume <session_id>]  # resume only; never on a fresh start
```

- `--print` + `--input-format stream-json` is what makes the CLI read *repeated* turns from stdin instead of
  one-shotting.
- `--verbose` is **required** for stream-json output, not optional.
- `--permission-mode bypassPermissions`: an interactive permission prompt would deadlock a headless child
  forever. This means an agent's tools are unrestricted *inside its sandbox* — the sandbox boundary is therefore
  the entire security story (ADVERSARY finding 002, accepted).
- **The prompt is passed as a file, never as argv.** `argv` is world-readable across uids, and the composed
  preamble contains injected ctx. The engine writes it into the node's `0700` config dir and passes the path.
- **`bypassPermissions` is refused when running as root**: exit 1 with *empty stdout* and
  `--dangerously-skip-permissions cannot be used with root/sudo privileges` on stderr. That exit is
  indistinguishable from an unauthenticated CLI, so children run **non-root with `IS_SANDBOX=1`**, and
  `needs_auth` is **never** inferred from an exit code alone — only from stderr or an explicit probe
  (`claude auth status --json`).

**Stdin, one line per turn**, newline-terminated, flushed, nothing else ever written:

```json
{"type":"user","message":{"role":"user","content":[{"type":"text","text":"<envelope>"}]}}
```

**Stdout parsing.** The engine matches only the event types it knows and treats everything else as opaque log
output. There is no exhaustive match and no "unexpected event" error path — the real CLI emits types that are not
in this document (`rate_limit_event`, `system/thinking_tokens` were observed), and an engine that pattern-matched
exhaustively would fall over in production.

| Event | Engine action |
|---|---|
| `system` / `init` | Record `session_id`, `model`; status → `idle`. |
| `assistant` | Append text to the log. |
| `user` | Tool results — append to the log. |
| `result` | **Turn complete.** Usage fields feed `state.spend` and the agent `budget`. `is_error` → status `error` + `last_error`; else in-flight message → `consumed`, status → `idle`, then ephemeral clear if configured, then deliver next queued. |
| anything else | Logged verbatim, ignored. |

A **non-JSON line on stdout is never fatal**: it is logged verbatim as a `stdout` line and the stream continues.
stderr is captured as `stream=stderr` and never parsed as JSON.

### Codex (`harness: "codex"`) — M2

Deferred. The auth spike established that `codex exec` is not a safe auth probe (it proceeds unauthenticated and
dies at request time with a 401) and that the API-key env var is **`CODEX_API_KEY`** — `OPENAI_API_KEY` is noticed
but is *not* in the auth resolution chain. Exact event names for `codex exec --json` are unverified; they will be
pinned with QA before M2 rather than guessed.

---

## 5. The `wheel` CLI

Yoke-shaped by PM decision (§3): every node is a keyspace, identity comes from the token and is never passed,
denial is **exit 3**. Reaches the engine at `WHEEL_ENGINE_URL` with `WHEEL_TOKEN`.

```
wheel whoami                          identity: name, id, type, position, wires both directions
wheel connections                     my wires, in plain language
wheel ls                              every keyspace I'm wired to, with wire type   (§3c#7)
wheel ls    <node> [prefix]           table row keys / chest paths
wheel msg   <agent> "<text>"|--stdin|--file <p>   → {id, sha256, bytes, state}      (§3c#3)
wheel read  <node>                    ctx markdown / table rows / chest listing
wheel read  <node>/<row>              table row JSON / chest blob (--out <file>)
wheel write <node> "<v>"|--stdin|--file           ctx: replace markdown
wheel write <node>/<row> "<v>"|--stdin|--file     table: upsert by key; chest: put blob
wheel rm    <node>/<row>              table row / chest blob (needs write)
wheel query <table> "<SELECT …>"      read-only SQL, scoped to that one table
wheel secret get <vault>/<key>        vault value
wheel run   <script> [args…]          invoke a script node; stdout returned
wheel inbox [--since <ts>] [--limit n] | wheel inbox <id>    re-read my messages    (§3c#2)
wheel ctx clear                       clear my own context
```

Every command prints one human line, or JSON with `--json`.

| Exit | Meaning |
|---|---|
| 0 | Success. |
| 1 | Usage / local error. |
| 2 | Engine error. |
| **3** | **Wire denied** — `no wire from <me> to <node> (need: write)`. |
| 4 | No such node. |

**Argv hazard warning** (§3c#1). A body passed as argv goes through the agent's shell, where backticks and `$(…)`
are substituted and the message is silently corrupted — this is a real defect observed on YOKE, and it is why
`msg`/`write` warn on stderr when a value contains `` ` `` or `$(` and point at `--file`/`--stdin`. The durable
fix is the built-in MCP server (`wheel mcp-serve`, M2) so agents call tools instead of shelling out; the preamble
tells them to prefer it.

---

## 5b. Idle parking and lifecycle (§3c#14)

Compute frugality is an operator directive, not an optimisation: one live process per agent forever is what
made YOKE unusable. So:

- After `idle_timeout_secs` (default 300, `0` disables) the supervisor **stops the process and keeps the
  session id**. Status `idle → parked`. The next message resumes with `--resume <session_id>`, so parking
  never loses context — except under `ephemeral_context`, where the context was being cleared anyway.
- A parked agent has **no live process**, and that is healthy. Nothing may treat process liveness as a proxy
  for agent health.
- Per-host cap on concurrently `running` agents (env, default 32) with a fair queue. `run_on_startup` starts
  agents **parked**, not running.
- `budget: {max_turns?, max_usd?}` → on reach, status `budget_exhausted`; the engine will not self-restart.
- The engine idles at ~0 CPU: no polling loops anywhere — channels, WS and inotify only.

Statuses: `stopped | starting | needs_auth | running | idle | parked | budget_exhausted | error`, plus
`hosted_on` (`"cloud"` | runner id | `null`). **`null` means unhosted, which is a loud, alarming state** —
an agent nobody can run is broken, and the UI says so rather than showing it as merely stopped.

## 6. Limits (§3c#6)

Enforced client-side *before* sending, so callers get a clear error rather than discovering a limit by failing —
and re-checked by the engine, which never trusts a child.

| Thing | Limit | On exceed |
|---|---|---|
| Message body | 256 KiB | `too_large`, refused at send |
| ctx markdown / table row value | 1 MiB | `too_large` |
| Chest blob | 50 MiB | `too_large` |
| Script output captured | 1 MiB | truncated **in the captured output only**, flagged in the result |
| Script runtime | `timeout_secs`, default 60, max 600 | `timeout`, process killed |
| Table query | 5 s | `timeout` |

Exceeding a limit never truncates a *message* (§3c#11).

---

## 7. Engine spawn contract (§4b)

`wheel-host` (owned by API) starts one engine per project. Names pinned in `wheel_core::spawn`.

| Env | Meaning |
|---|---|
| `WHEEL_PROJECT_ID` | uuid of the project this engine serves |
| `WHEEL_ENGINE_SECRET` | control-plane bearer |
| `WHEEL_VAULT_KEY` | base64 per-project key for vault encryption at rest |
| `WHEEL_DATA_DIR` | sqlite db, chest blobs, scripts, creds |
| `WHEEL_LISTEN` | `tcp://0.0.0.0:7000` or `unix:///run/wheel/<id>/engine.sock` |
| `WHEEL_LOG` | `json` for structured logs |
| `WHEEL_ROLE` | `engine` (default) or `host` — one image ships both |

Guarantees the engine makes to the host:

- `/healthz` answers `200` within **10 s** of spawn.
- Misconfiguration → **non-zero exit with a one-line reason on stderr**, immediately. It never starts half-configured.
- `SIGTERM` → stops children, flushes sqlite, exits within **15 s**.
- In `process` mode it runs as the uid the host already dropped to; it does not attempt privilege changes itself.

Children are spawned with `WHEEL_TOKEN_FILE`, `WHEEL_ENGINE_URL`, `WHEEL_NODE`, plus the keys of every wired
vault. Never `WHEEL_ENGINE_SECRET`.

**The capability token is passed as a `0600` file, not an environment variable** (ADVERSARY finding 007).
`/proc/<pid>/environ` is readable by the same uid, so an env token would hand every co-resident child every
other child's authority. Paired with a **per-node uid** (each child gets its own uid from the project's range;
the engine holds ambient `CAP_SETUID`/`CAP_SETGID` only; credential dirs are `0700`; shared workspaces are
setgid), a file only the token's own uid can read is the real boundary.

**Gap, stated deliberately:** per-node uid separation lands in **M2** for the docker backend and **M3** for the
process backend. Until then all of a project's children share one uid, so the token file is a defence in depth
rather than a boundary, and a compromised child can reach its siblings' tokens. This is a known, accepted,
time-boxed gap — not an oversight.

### Forged harness events (ADVERSARY finding 008)

An agent controls its own stdout, so it can print a line that looks like a top-level `result` event and try to
end its own turn early, skip accounting, or desynchronise the delivery loop. Two rules:

- A `result` is accepted as turn-complete only when its `session_id` matches the session the supervisor
  started. Forged or foreign events are logged and ignored.
- **Budget and turn counting are enforced supervisor-side**, from the supervisor's own record of turns
  delivered — never from usage numbers reported by the child.

---

## 8. Test images

`make engine-image` → `wheel-engine:dev` (production layout).
`make engine-image-test` → `wheel-engine:test` — identical, plus QA's `qa/harness/fake-claude` and `fake-codex`
shadowing `/usr/local/bin/{claude,codex}`. The fakes speak the same stream-json protocol, so **the engine cannot
tell the difference** and tests exercise the real supervision path rather than a mock of it. The fakes are absent
from the production image and a make target asserts it.
