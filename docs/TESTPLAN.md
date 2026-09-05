# Wheel — Test Plan & Acceptance Criteria

Owner: QA. Source of truth for *what "done" means*, derived line-by-line from
`docs/ARCHITECTURE.md`. If a criterion here contradicts the architecture doc, the architecture doc
wins and this file is the bug — tell me and I'll fix it.

**Every criterion has an ID.** Tests reference the ID; bug reports quote the ID; `qa/BUGS.md`
tracks failures by ID. If you're implementing something and want to know when it's finished, find
its ID here.

## 0. Conventions

**ID scheme:** `<AREA>-<thing>-<case>`, e.g. `WM-agent-ctx-read`, `API-auth-owner-404`.

**Areas:** `NODE` data model · `WM` wire matrix · `TOOL` tool nodes (§3d) · `MSG` message delivery · `INJ` injection &
ephemeral context · `ENG` engine control plane · `CLI` the `wheel` binary · `API` public API ·
`ING` ingress · `SEC` isolation & secrets · `COMMS` §3c comms hardening · `E2E` browser ·
`PERF` soak · `BACK` sandbox backends.

**Severity** (used in `BUG:` reports): **S1** data loss / security / privilege escalation ·
**S2** spec violation · **S3** wrong but has a workaround · **S4** polish.

**Status legend** in the tables: `todo` not implemented · `red` implemented, failing ·
`green` passing · `n/a` deferred past M3.

**Hermeticity is non-negotiable.** No test in CI touches the real Anthropic/OpenAI APIs. The fake
harness (`qa/harness/`, image `wheel-engine:test`) stands in. `make test-live` is opt-in, run by
humans, never in CI.

**A denied operation must be denied in BOTH places.** API-side validation and engine-side
validation are independent defences; a test that only checks the API would pass while the engine
is wide open to anything that reaches it directly. Every `deny` case below is asserted twice.

---

## 1. NODE — data model & validation (§3)

| ID | Criterion |
|---|---|
| `NODE-name-charset` | Name matches `^[a-z0-9][a-z0-9-_]{0,62}$`. Reject: leading `-`/`_`, uppercase, spaces, empty, 64+ chars, unicode homoglyphs, `..`, `/`. |
| `NODE-name-unique` | Name unique per project; duplicate create → 409, and the existing node is untouched. |
| `NODE-name-rename-table` | Renaming a `table` node renames sqlite table `t_<old>` → `t_<new>`; data survives; old name is gone. |
| `NODE-name-rename-collide` | Rename onto an existing name → 409, no partial rename (node name AND `t_` table both unchanged). |
| `NODE-type-closed` | `type` outside the 8 known values → 400. |
| `NODE-config-tagged` | `config` is validated against the node's `type`; a `ctx` config on an `agent` node → 400. |
| `NODE-config-unknown-key` | Unknown key in `config` → 400 (fail closed; silently dropping config is how features get "implemented" and never run). |
| `NODE-state-not-config` | `status`, `session_id`, `last_activity`, `last_error` are reported in `state` and are NOT accepted as `config` input; attempting to set them is ignored or 400, never persisted. |
| `NODE-position-float` | `position.x/y` accept floats incl. negative; round-trip without precision loss. |
| `NODE-delete-cascade` | Deleting a node deletes all wires referencing it in BOTH directions; no orphan wire rows remain. |
| `NODE-delete-table-drops` | Deleting a `table` node drops `t_<name>`. |
| `NODE-delete-chest-dir` | Deleting a `chest` node removes `/data/chest/<node_id>/` and all blobs. |
| `NODE-delete-vault-keys` | Deleting a `vault` node destroys its ciphertext; keys are unrecoverable afterwards. |
| `NODE-agent-config` | `agent`: `harness ∈ {claude, codex}`, `system_prompt` required, `run_on_startup`/`ephemeral_context` bool, `model` optional-nullable. Bad harness → 400. |
| `NODE-table-columns` | `table`: column `type ∈ {text,integer,real,blob,json}`; bad type → 400; column names validated against the same charset rule (SQL injection via column name → 400). |
| `NODE-endpoint-path` | `endpoint`: `path` must lead with `/`, contain no `..`, no `//`, no null byte; `method ∈ {GET,POST,PUT,DELETE}`; `response_mode ∈ {ack,script}`. |
| `NODE-endpoint-auth` | `auth` is `{mode:"none"}` or `{mode:"bearer", vault_ref}`; `bearer` without `vault_ref` → 400; any other mode → 400. |
| `NODE-endpoint-path-collide` | Two endpoints with the same method+path → 409. |
| `NODE-script-lang` | `script`: `language ∈ {python,ts,js}`; `timeout_secs` defaults to 60, must be > 0 and ≤ a documented ceiling. |
| `NODE-mcp-transport` | `mcp`: `stdio` requires `command`; `http` requires `url`; supplying both/neither → 400. |
| `NODE-tool-config` | `tool`: `kind: "http"`, valid `base_url`, `operations[]` each with unique slug `id`, method, path, `enabled`, and a `Fill` for every param/body field. Bad `fill.mode` → 400. |
| `NODE-vault-writeonly` | `vault.config.keys` lists key NAMES only; values are never accepted here. |
| `NODE-schema-roundtrip` | Every node type round-trips create → `GET /v1/board` → update → read without field loss or type coercion. |
| `NODE-state-always-present` | `GET /v1/board` returns every node as `{...node, state}` with `state` **always present** — `null` for non-agent types, never omitted. A consumer must not have to distinguish "absent" from "null". |

---

## 2. WM — wire matrix (§3), exhaustive

All **243** cells (9 node types × 9 × 3 wire types) are enumerated in
**`qa/fixtures/wire_matrix.json`**, generated from the contract by
`qa/tools/gen_wire_matrix.py`. **26 allow, 217 deny.** `make check` fails if the fixture drifts
from the generator, so the doc, the fixture and the tests can't disagree.

IDs are `WM-<from>-<to>-<type>`, e.g. `WM-agent-ctx-read` (allow),
`WM-table-agent-send` (deny). `tool`'s and `endpoint`'s only outgoing `read` wire is → `vault`.

| ID | Criterion |
|---|---|
| `WM-create-allow` | Each of the 26 allowed cells: `POST /v1/wires` → 201, wire appears in `GET /v1/board` on the FROM node's outgoing list. |
| `WM-create-deny` | Each of the 217 denied cells: `POST /v1/wires` → 4xx with a machine-readable reason; no wire row is created. |
| `WM-engine-deny` | Same 217, posted directly to the **engine** control plane, bypassing the API → rejected. Independent defence. |
| `WM-enforce-runtime` | For each allowed cell, the corresponding `wheel` CLI call succeeds; for each denied cell, it fails with **exit 3** and touches nothing. |
| `WM-write-implies-read` | A `write` wire to table/chest grants read too (`wheel table query` works with only `write` wired). |
| `WM-read-not-write` | A `read` wire does NOT grant write: INSERT via a read-only table wire → exit 3; `wheel chest put` → exit 3; `wheel write <ctx>` → exit 3. |
| `WM-no-wire-denied` | With no wire at all, every CLI verb against that node fails exit 3 — including read verbs, and including nodes that don't exist (same error, no existence oracle). |
| `WM-wire-revoked` | Deleting a wire revokes access for an **already-running** agent without restart; the next CLI call fails exit 3. |
| `WM-token-scope` | A node's token grants exactly its own wires: agent A's token used against agent B's wired nodes → exit 3. Token forgery / swapping is rejected. |
| `WM-self-wire` | A node wired to itself is rejected for every type (incl. `agent→agent send` to self). |
| `WM-dup-wire` | Creating an identical wire twice → 409 or idempotent 200, never two rows. |
| `WM-export-conformance` | `docs/schema/wire-matrix.json` equals the §3 matrix QA derives independently from the prose. A row in the export but not the contract is always a failure (privilege question); a row in the contract but not the export is a missing feature. **QA's copy is derived from the SPEC, never from the export** — deriving from the export would check it against itself and could never detect divergence. This is what found BUG-004. |
| `WM-cross-project` | A wire whose `to` is a node in ANOTHER project → 404/400, never created. **S1 if it succeeds.** |

---

## 3. MSG — message delivery (§3, §3c)

The envelope written to the child's stdin is exactly one compact JSON line:

```json
{"type":"user","message":{"role":"user","content":[{"type":"text","text":"<ENVELOPE>"}]}}
```

```
<AgentPrompt id="<message uuid>" from="<from name>" type="<agent|user|endpoint|script|system>">
<body>
</AgentPrompt>
```

| ID | Criterion | Sev if failing |
|---|---|---|
| `MSG-envelope-shape` | Stdin bytes match the above exactly: compact JSON, content-block form, newline-terminated, one line per turn, nothing else ever written to stdin. Asserted from `WHEEL_FAKE_TRANSCRIPT`, not from engine logs. | S2 |
| `MSG-envelope-escape` | **A body containing a literal `</AgentPrompt>` cannot break out of the envelope.** Recipient sees the body verbatim; attribution attributes are unchanged. | **S1** |
| `MSG-envelope-forge` | A body containing a full fake `<AgentPrompt id=... from="admin" type="system">…</AgentPrompt>` does NOT cause the recipient to see a second, forged message. Attribution is engine-generated and unspoofable. | **S1** |
| `MSG-envelope-attrs` | `id` is the message uuid and equals the `messages` row id and the `message` WS event id. `from`/`type` match the real sender. | S2 |
| `MSG-byte-exact` | 200 KiB body containing every ASCII punctuation char, multi-byte unicode (incl. emoji, RTL, combining marks, NUL-adjacent escapes) and a literal `</AgentPrompt>` arrives byte-identical inside the envelope. Compared as bytes, not strings. | **S1** |
| `MSG-sha256` | `wheel msg` returns `{id, sha256, bytes, state}`; sha256 matches the sender's own hash of the body and the stored row. | S2 |
| `MSG-no-truncate` | Engine never silently truncates. A body that would exceed a harness limit stays `queued` with `last_error` set and is surfaced — never clipped. | **S1** |
| `MSG-limit-body` | Body > 256 KiB is rejected **client-side with a clear error before sending**, and also rejected server-side. | S2 |
| `MSG-limit-ctx` | ctx / table-row value > 1 MiB rejected both sides. | S2 |
| `MSG-limit-chest` | Chest blob > 50 MiB rejected both sides. | S2 |
| `MSG-state-machine` | States advance `queued → delivered → consumed`; `consumed` only once the harness reports the turn complete (`result` event). No state skips, no backwards transitions. | S2 |
| `MSG-state-event` | Every transition emits a `message` WS event carrying the same id. | S3 |
| `MSG-queue-stopped` | Messages to a stopped agent persist as `queued` and are not lost across engine restart. | **S1** |
| `MSG-queue-drain-order` | On start, the queue drains in `created_at` order, one at a time. | S2 |
| `MSG-one-in-flight` | The next message is written to stdin only after the previous turn completes; never two turns in flight. | S2 |
| `MSG-inbox-list` | `wheel inbox [--since] [--limit]` lists received messages. | S2 |
| `MSG-inbox-reread` | `wheel inbox <id>` returns the body **byte-identical to what was written into the transcript** — a garbled delivery is recoverable. | S2 |
| `MSG-inbox-scope` | An agent's inbox shows only ITS messages. Reading another node's inbox → exit 3. | **S1** |
| `MSG-from-user` | UI chat messages arrive with `from_node = user`, envelope `type="user"`. | S3 |
| `MSG-durable-restart` | `messages` rows (id, sha256, bytes, state, timestamps, last_error) survive container restart. | S2 |
| `MSG-error-turn` | `result.is_error=true` → agent `status=error`, `last_error` = `result.result`; the message is marked `consumed` with `error=true`. | S2 |
| `MSG-poison-once` | **A message is consumed exactly once and never redelivered**, even when its turn errored. Assert across a restart: a poison message must not resurrect and loop forever. | **S1** |
| `MSG-fairness-user-cap` | The user lane drains at most **3 consecutive** user messages before one normal-lane message is delivered. | S2 |
| `MSG-fairness-aging` | A normal-lane message older than **60 s** is promoted to the front, so user chatter cannot starve agent traffic indefinitely. | S2 |
| `MSG-priority-lane` | User messages are ordered ahead of queued agent/endpoint/script messages, but are never injected mid-turn (single writer). | S2 |

### 3a. Single writer & the user priority lane (§3c #12)

The engine's per-agent delivery loop is the **only** thing that ever writes to a child's stdin.
User messages take a priority lane ahead of queued agent/endpoint/script messages, but are
**never** injected mid-turn. All of these are asserted from `WHEEL_FAKE_TRANSCRIPT` — the raw
bytes the child received — because the engine's own view of what it sent is the thing under test.

| ID | Criterion | Sev |
|---|---|---|
| `MSG-single-writer` | The transcript is always a sequence of **whole JSON lines**. No interleaved, split or partial writes, ever — under concurrent sends, ingress hits, script sends and UI sends at once. Asserted by parsing every transcript line strictly and byte-counting. | **S1** |
| `MSG-no-midturn` | Agent is mid-turn (scripted slow turn via `WHEEL_FAKE_SCRIPT` `{"sleep":N}`); a user message sent during it appears in the transcript **only after** that turn's `result` event. Not one byte earlier. | **S1** |
| `MSG-priority-user` | With 3 agent messages already queued and 1 user message sent after them, the **user's is delivered first**; the 3 then drain in their original order. | S2 |
| `MSG-priority-order` | Two rapid user sends arrive **in send order**, each as its own turn — the priority lane is FIFO within itself, not a stack. | S2 |
| `MSG-priority-no-starve` | A steady stream of user messages does not starve queued agent messages forever; document and assert the fairness rule. | S3 |
| `MSG-queued-next` | The message the delivery loop will send next reports state `queued (next)`, so the UI can show the user exactly when their message lands. | S3 |
| `MSG-stdin-sole-path` | No path other than the delivery loop can reach a child's stdin — not the log/exec routes, not the built-in MCP server, not script nodes. Asserted by attempting each. | **S1** |
| `MSG-interrupt` | `POST /v1/agents/:id/interrupt` cancels the in-flight turn per the harness protocol and then delivers the user's message. Never implicit — no other action interrupts a turn. *(M2)* | S2 |
| `MSG-draft-local` | The chat box draft is client-side only (localStorage per agent, survives reload) and creates no `messages` row until Send. *(Web)* | S3 |

---

## 4. INJ — ctx injection & ephemeral context (§3)

| ID | Criterion |
|---|---|
| `INJ-on-start` | ctx markdown from every `ctx→agent send` wire is present in the composed system prompt at start — asserted from the fake's **first event**, i.e. what the child actually received. |
| `INJ-multi-ctx` | Multiple wired ctx nodes are all injected as `\n\n# Context: <ctx name>\n<markdown>`, **ordered by ctx node name in byte order** — stable and board-position-independent. Asserted by wiring ctx nodes created out of order and checking the composed prompt. |
| `INJ-after-clear` | Injection is re-applied after every context clear, not just the first start. |
| `INJ-edit-visible` | Editing a ctx node's markdown changes what the next start/clear injects. |
| `INJ-unwired-absent` | A ctx node NOT wired to the agent never appears in its prompt. **S1** if it leaks. |
| `INJ-system-prompt` | The agent's configured `system_prompt` is present alongside injected ctx, in the documented order. |
| `INJ-ephemeral-clears` | With `ephemeral_context: true`, session_id changes after the turn completes. |
| `INJ-ephemeral-reapplies` | After the clear, system prompt + ctx injection reappear in the new session's first event. |
| `INJ-ephemeral-then-drain` | Queue draining resumes after the clear; no message is dropped or double-delivered across the boundary. |
| `INJ-ephemeral-off` | With `ephemeral_context: false`, session_id is stable across turns. |
| `INJ-ctx-clear-cli` | `wheel ctx clear` from inside the agent performs the same clear+reapply. |
| `INJ-run-on-startup` | `run_on_startup: true` starts the agent when the container starts; `false` does not. |

---

## 5. ENG — engine control plane (§4)

Contract-tested one request per documented route in `docs/PROTOCOL.md` against a real engine
container with an empty board (`ENG-route-*`), then behaviourally.

| ID | Criterion |
|---|---|
| `ENG-route-exists` | Every route documented in PROTOCOL.md exists — distinguishing 404 (missing) from 405 (wrong method). A documented-but-absent route is a doc bug or a code bug; either way it's a bug. |
| `ENG-route-undocumented` | The engine exposes no route that PROTOCOL.md doesn't document (attack surface must be documented surface). |
| `ENG-auth-required` | Every `/v1/*` route without the bearer `WHEEL_ENGINE_SECRET` → 401. |
| `ENG-auth-wrong` | Wrong secret → 401, constant-time compare, no timing oracle. |
| `ENG-cli-token` | `/v1/cli/*` accepts a per-node token, NOT the engine secret; engine secret on a CLI route → 401 and vice versa. |
| `ENG-board-shape` | `GET /v1/board` returns `{nodes: [Node+state], project}` matching the exported JSON Schema. |
| `ENG-patch-partial` | `PATCH /v1/nodes/:id` with one field leaves the others untouched. |
| `ENG-events-ws` | `GET /v1/events` streams `node.state`, `message`, `log`, `board.changed`; each has the documented shape. |
| `ENG-events-replay` | `GET /v1/agents/:id/log?since=<cursor>` is consistent with what the WS streamed; no gaps, no dupes. |
| `ENG-log-garbage` | A non-JSON line on the harness's stdout is logged verbatim and is **never fatal** (`<<FAKE:GARBAGE>>`). |
| `ENG-log-unknown-event` | Unknown harness event types are logged opaquely and never error (`<<FAKE:NOISE>>` — the real CLI emits `rate_limit_event` and `system/thinking_tokens`, which PROTOCOL.md does not list). |
| `ENG-log-stderr` | Harness stderr is captured with `stream=stderr` and never parsed as JSON. |
| `ENG-child-crash` | `<<FAKE:CRASH>>` (SIGKILL) → agent goes to `error`, engine stays up, restart works. |
| `ENG-child-exit` | Non-zero exit mid-stream → `status=error`, `last_error` populated. |
| `ENG-auth-states` | `/auth/begin` → `/auth/complete` moves an agent out of `needs_auth`; `GET /auth` reports truthfully. Driven by `WHEEL_FAKE_AUTH`. |
| `ENG-lifecycle` | start/stop/restart/clear are idempotent; double-start doesn't spawn two children; stop kills the child and reaps it (no zombies). |
| `ENG-spawn-env` | §4b: engine starts with `WHEEL_PROJECT_ID`, `WHEEL_ENGINE_SECRET`, `WHEEL_VAULT_KEY`, `WHEEL_DATA_DIR`, `WHEEL_LISTEN`, `WHEEL_LOG=json`; missing/invalid any of them → **exit non-zero within 10s with a one-line reason**, not a hang. |
| `ENG-spawn-healthy` | `GET /healthz` is 200 within 10s of start; the host's `start` blocks until green or 504s at 30s. |
| `ENG-spawn-listen` | `WHEEL_LISTEN=tcp://…` and `unix://…` both work; in process mode the socket is owned by the project uid, mode-restricted, and **not** reachable over TCP. |
| `ENG-sigterm` | SIGTERM stops children and flushes sqlite within 15s, exits 0; no data loss, no orphaned harness processes. |
| `ENG-restart-persist` | Board, messages, table data and chest blobs all survive an engine restart. |

### 5a. Agent lifecycle — statuses, idle parking, and the root trap (§3c #13/#14)

Statuses: `stopped | starting | needs_auth | running | idle | parked | budget_exhausted | error`.

| ID | Criterion | Sev |
|---|---|---|
| `ENG-root-refusal` | Running the harness as **uid 0** with `--permission-mode bypassPermissions` is refused by the real CLI: exit 1, **empty stdout**, stderr `--dangerously-skip-permissions cannot be used with root/sudo privileges`. The engine must report a **configuration error**, NOT `needs_auth`. The exit code is identical to an unauthenticated CLI, so anything inferring auth state from the exit code alone will report `needs_auth` forever for a privilege misconfiguration — an unfixable-looking bug. Driven by `WHEEL_FAKE_ROOT=1`, no root container needed. | S2 |
| `ENG-nonroot-child` | Children are spawned non-root and with `IS_SANDBOX=1`. | **S1** |
| `ENG-needs-auth-source` | `needs_auth` is derived from stderr or an explicit probe (`claude auth status --json`), never from the exit code alone. Asserted by making a non-auth failure exit 1 and checking the agent does NOT land in `needs_auth`. | S2 |
| `ENG-park-idle` | After `idle_timeout_secs` (default 300) an idle agent transitions `idle → parked` and its harness **process is stopped**. A parked agent having no live process is correct, not a crash. | S2 |
| `ENG-park-resume` | The next message resumes a parked agent (`parked → starting → running`) with `--resume`, and the **same `session_id`** comes back — proving context was preserved rather than silently reset. | S2 |
| `ENG-park-no-loss` | A message that arrives while parked is not lost and is delivered exactly once after resume. | **S1** |
| `ENG-park-ephemeral` | With `ephemeral_context: true`, parking does not resurrect context that was meant to be cleared. | S2 |
| `ENG-one-process` | **Exactly one harness process per agent at any time.** 10 messages sent within 100 ms produce ONE process and 10 sequential turns. Start is idempotent: a second start while running is a no-op returning the existing session. | **S1** |
| `ENG-budget` | `budget.max_turns` / `max_usd` exhaustion → `budget_exhausted`, process stopped, event emitted; no further turns run. | S2 |
| `ENG-health-not-liveness` | No test (and no engine health check) uses "a process is alive" as a proxy for "the agent is healthy" — `parked` is healthy and processless. | S3 |

---

## 6. CLI — the `wheel` binary (§3c)

Grammar mirrors `yoke`. **Denial is exit code 3** throughout, so scripts can branch on it.

| ID | Criterion |
|---|---|
| `CLI-exit-0` | Success → exit 0. |
| `CLI-exit-3-denied` | **Every wire-denied operation exits 3** with a message naming the node and the missing wire type — distinct from exit 1 (usage/runtime error) and exit 2 (bad arguments). |
| `CLI-exit-nonexistent` | An operation on a node that doesn't exist is indistinguishable from one that's merely unwired: same exit 3, same message. No existence oracle. **S1** if distinguishable. |
| `CLI-msg-argv-warn` | `wheel msg <to> <body...>` warns **on stderr** when the body contains a backtick or `$(`, pointing at `--file`. (This bit us on YOKE: shell substitution silently beheaded messages.) |
| `CLI-msg-file` | `wheel msg <to> --file <path>` and `--stdin` send the body byte-exactly, no shell involvement. |
| `CLI-msg-returns` | `wheel msg` prints `{id, sha256, bytes, state}`. |
| `CLI-msg-no-from` | There is no `--from`; sender is derived from the process token and cannot be spoofed by any flag or body field. **S1** if spoofable. |
| `CLI-ls-bare` | `wheel ls` with no argument lists every keyspace the caller is wired to, with wire type. (YOKE made this operator-only; agents couldn't enumerate their own capabilities.) |
| `CLI-connections` | `wheel connections` explains each wire in plain language. |
| `CLI-whoami` | `wheel whoami` reports the node's own name, type and id. |
| `CLI-read-write` | `wheel read/write <ctx>`, `--file` form, respects read vs write wires. |
| `CLI-table-query` | `wheel table query` runs SELECT; non-SELECT via a read-only wire → exit 3. |
| `CLI-secret-get` | `wheel secret get <vault>/<key>` works with a read wire; unwired → exit 3. |
| `CLI-chest-verbs` | `get|ls|put|rm` respect read/write; `..` in a key → error, never escapes the chest dir. |
| `CLI-run` | `wheel run <script>` returns stdout, stderr and exit code faithfully. |
| `CLI-inbox` | `wheel inbox` / `wheel inbox <id>` per `MSG-inbox-*`. |
| `CLI-limits-clientside` | Over-limit bodies are refused locally with a clear message **before** any network call. |
| `CLI-ceiling-rows` | `wheel read <table>` / `query` return at most **10,000 rows** per call; more requires `--limit/--offset`. The cap is applied, not silently truncating a claimed-complete result. |
| `CLI-ceiling-keys` | `wheel ls` returns at most **10,000 keys**, paged the same way. |
| `CLI-ceiling-timeout` | `script.timeout_secs` > 300 is refused. |

---

## 7. API — public API (§5)

| ID | Criterion | Sev |
|---|---|---|
| `API-auth-missing` | No `x-auth-token` → 401. | S2 |
| `API-auth-invalid` | Malformed / bad-signature / expired JWT → 401. | S2 |
| `API-auth-alg-none` | `alg: none` or an HS256-signed token where RS256 is expected → 401. | **S1** |
| `API-auth-wrong-key` | Token signed by a different key that isn't in Clerk's JWKS → 401. | **S1** |
| `API-auth-order` | Order is verify JWT → load project → assert `owner_id == jwt.sub` → act. Asserted by timing/behaviour: an invalid token against someone else's project must 401 (not 404), proving verification happens first. | S2 |
| `API-auth-owner-404` | A valid token for a project owned by someone else → **404**, byte-identical to a nonexistent project. No enumeration oracle (status, body, or timing). | **S1** |
| `API-project-crud` | create/list/get/patch/delete round-trip; `GET /v1/projects` works without `x-project-id`. | S2 |
| `API-project-delete` | Delete stops and removes the container AND the volume; no orphan resources. | S2 |
| `API-lifecycle` | start/stop/restart reflect in `status`; idempotent. | S3 |
| `API-proxy-auth` | `/v1/projects/:id/engine/*` enforces the same auth+ownership before proxying, and never leaks `WHEEL_ENGINE_SECRET` to the client. | **S1** |
| `API-proxy-ws` | WS `/engine/v1/events` proxies bidirectionally and closes cleanly when the container stops. | S3 |
| `API-route-parity` | Every route in `docs/API.md` exists (404-vs-405 probe) and no undocumented route does. | S3 |
| `API-dev-interlock-boot` | The dev-auth bypass is interlocked at BOOT, not per request: an API started with `AUTH_DEV_SECRET` set while `WHEEL_ENV != dev` **exits non-zero**. Asserted for `WHEEL_ENV` unset (unset counts as prod — the permissive default is the one that kills you), for a typo (`development`), and for prod. Empty `AUTH_DEV_SECRET` is treated as absent, never as an empty HMAC key. API owns `config_interlock.rs`; QA asserts it independently, from outside the process, because a guarantee this load-bearing should not be checked only by the code that makes it. | **S1** |
| `API-dev-token-hermetic` | The HS256 + `AUTH_DEV_SECRET` path (claims `{sub, iss, exp, nbf}`, `iss` matching `CLERK_ISSUER` exactly) authenticates in dev mode. Two different `sub` values are two tenants — this is how `API-auth-owner-404` is tested without Clerk. Mint via `infra/dev/e2e.py:mint()` (API's implementation, reused not rewritten). | S2 |
| `API-alg-confusion` | In prod any non-RS256 `alg` is rejected on sight, including HS256 signed with the RSA **public** key. | **S1** |
| `API-healthz` | `GET /healthz` needs no auth and reports dependency health honestly. | S4 |

---

## 8. ING — ingress (§2, §5)

| ID | Criterion | Sev |
|---|---|---|
| `ING-cap-off` | With capability `http: false`, `ANY /p/:id/*` → 403 and **nothing reaches the container**. | **S1** |
| `ING-cap-on` | With `http: true`, the request reaches the matching endpoint node. | S2 |
| `ING-cap-toggle` | Toggling the capability takes effect without restarting the project. | S3 |
| `ING-no-auth` | Ingress is public by design — assert it needs no Clerk token, and that it CANNOT reach `/v1/*` control-plane routes. | **S1** |
| `ING-endpoint-match` | Method+path routes to the right endpoint node; unmatched → 404. | S2 |
| `ING-traversal` | `/p/<id>/../v1/board`, encoded (`%2e%2e`), double-encoded, and backslash variants all fail to reach the control plane. | **S1** |
| `ING-to-agent` | `endpoint→agent send`: the hit is delivered as a message with method, path, header subset and body; envelope `type="endpoint"`. | S2 |
| `ING-to-table` | `endpoint→table write`: JSON body inserted as a row; malformed JSON → 400, no partial row. | S2 |
| `ING-to-script-ack` | `response_mode: ack` returns immediately without waiting for the script. | S3 |
| `ING-to-script-body` | `response_mode: script` returns the script's stdout as the response body. | S3 |
| `ING-auth-bearer` | With `auth.mode="bearer"`, a request with no / wrong bearer → **401 with no body** (no oracle about the expected token); correct bearer passes. | **S1** |
| `ING-auth-bearer-wire` | `bearer` requires an `endpoint→vault read` wire; without it the endpoint fails closed (401), never open. | **S1** |
| `ING-auth-bearer-timing` | Bearer comparison is constant-time; no timing oracle. | S2 |
| `ING-ratelimit` | Documented rate limit is enforced and returns 429. | S3 |
| `ING-header-filter` | Only the documented header subset is forwarded; `Authorization`/cookies are not leaked into the agent's prompt. | **S1** |

---

## 8b. TOOL — tool nodes (§3d, M2)

Imported HTTP specs become agent-callable operations. Two things make this security-sensitive: the
engine makes outbound requests on the user's behalf (SSRF), and it injects secrets into them that
the calling agent must never see (fill precedence).

### Import & normalisation

| ID | Criterion | Sev |
|---|---|---|
| `TOOL-import-formats` | The **same API** described as OpenAPI 3.x, Swagger 2, Postman v2.1 and Insomnia v4 normalises to **identical** `operations[]` — same ids, methods, paths, params. One fixture API, four source files, one expected output. | S2 |
| `TOOL-import-detect` | Format auto-detected when `format` is omitted; an explicit wrong `format` fails loudly rather than mis-parsing. | S3 |
| `TOOL-import-preview` | `POST /v1/tools/import` returns normalised ops and creates **no node**. | S2 |
| `TOOL-import-malformed` | Truncated / non-JSON / wrong-schema / hostile-huge documents → 400, never a panic, never partial state. | S2 |
| `TOOL-import-idempotent` | Re-import diffs by `method+path`, **keeps existing fills**, flags added/removed ops. A re-import must never silently reset a `vault` fill back to `agent`. | **S1** |
| `TOOL-import-engine-only` | The engine is the only parser; Web calls the endpoint rather than re-implementing it (assert Web has no spec-parsing code path). | S3 |
| `TOOL-op-slug-unique` | Operation `id` slugs are unique within a node; collisions from distinct paths are disambiguated deterministically. | S2 |

### Fills & agent exposure — the privilege boundary

| ID | Criterion | Sev |
|---|---|---|
| `TOOL-ls-agent-fields-only` | `wheel tool ls` / `GET /v1/tools/:id/ops` / the MCP input schema expose **only** `agent`-mode fields. `static`, `vault` and `hidden` fields are absent from the schema entirely — not present-but-masked. | **S1** |
| `TOOL-reject-extra-field` | An agent supplying a field that is not `agent`-mode → **400**, logged as a denial event. Covers static, vault, hidden, and fields not in the spec at all. | **S1** |
| `TOOL-fill-precedence` | `vault`/`static` are authoritative: an agent cannot override them by any means — same-name arg, differing case, duplicate key, nested JSON pointer collision, array-index path, or header smuggling. | **S1** |
| `TOOL-vault-requires-wire` | A `vault`-mode fill without a `tool→vault (read)` wire fails the call; it does **not** fall back to sending the request without the secret, nor to an empty value. | **S1** |
| `TOOL-vault-not-in-board` | Resolved vault values never appear in `GET /v1/board`, the node config, WS events, the call log, or error messages. Canary-grep every response. | **S1** |
| `TOOL-curl-masked` | `--curl` / UI "copy as curl" renders the exact equivalent request with static **and** vault values masked. | **S1** |
| `TOOL-hidden-omitted` | `hidden` fields are omitted from the outbound request entirely — not sent empty, not sent null. | S2 |
| `TOOL-log-no-secrets` | The per-call event logs `{tool, op, status, duration_ms, bytes}` and never request bodies, headers or resolved secrets. | **S1** |

### Execution & SSRF

| ID | Criterion | Sev |
|---|---|---|
| `TOOL-ssrf-base-url` | `base_url` resolving to loopback, RFC1918, link-local (169.254/fd00::/::1), `*.railway.internal`, `*.internal`, or a host-local address is **denied at import and at call time**. | **S1** |
| `TOOL-ssrf-redirect` | A public URL that 30x-redirects to a denied address is blocked **at the redirect**, not just at the first hop. Chain depth ≤ 3 enforced. | **S1** |
| `TOOL-ssrf-dns` | A public hostname whose DNS resolves to a private address is denied — resolution is checked against the **connected** address, defeating DNS rebinding between check and connect. | **S1** |
| `TOOL-ssrf-encodings` | Decimal/octal/hex IP encodings, IPv4-mapped IPv6, trailing dots and userinfo tricks (`http://public@127.0.0.1/`) are all denied. | **S1** |
| `TOOL-timeout` | 30s timeout enforced; a hanging upstream fails the call and does not wedge the agent's turn. | S2 |
| `TOOL-response-cap` | Responses > 5 MiB are truncated-with-error or rejected, never buffered unbounded. | S2 |
| `TOOL-call-shape` | `wheel tool call` returns `{status, headers, body}`; non-2xx upstream is returned faithfully, not swallowed. | S2 |
| `TOOL-dry-run` | `POST /v1/tools/:id/call {dry_run:true}` returns the curl string without sending. | S3 |
| `TOOL-disabled-op` | A disabled op is not listed and cannot be called. | S2 |
| `TOOL-mcp-exposure` | Each `read`-wired tool contributes enabled ops to the built-in MCP server as `<tool>__<op>`, description = summary, input schema = agent fields only. Unwiring removes them at next start. | **S1** |
| `TOOL-wire-gated` | `wheel tool ls/call` without an `agent→tool read` wire → exit 3. Scripts likewise (`script→tool read`). | **S1** |

---

## 9. SEC — isolation & secrets

| ID | Criterion | Sev |
|---|---|---|
| `SEC-vault-never-read` | Vault values never appear in `GET /v1/board`, any node/config response, the WS stream, or any log line. Asserted by writing a canary value and grepping every response body and the whole log. | **S1** |
| `SEC-vault-write-only` | `PUT /v1/vault/:id/:key` is the only way in; there is no read route. | **S1** |
| `SEC-vault-at-rest` | Values are encrypted at rest with a per-project key; the canary does not appear in raw `/data/wheel.db` bytes. | **S1** |
| `SEC-vault-env-scope` | Vault keys are exported into the env of agents **wired to that vault only**; an unwired agent's env has neither the key nor the value. | **S1** |
| `SEC-table-isolation` | `wheel table query` cannot read another table node's `t_` table, or any engine-internal table (`nodes`, `wires`, `messages`, vault ciphertext) — via plain name, `ATTACH`, `pragma`, `sqlite_master`, CTE, or subquery. | **S1** |
| `SEC-table-readonly` | A read wire cannot mutate: INSERT/UPDATE/DELETE/DROP/ATTACH/pragma writes all rejected. | **S1** |
| `SEC-table-injection` | Table and column names derived from node names cannot inject SQL (`NODE-name-charset` is the defence; assert the failure mode too). | **S1** |
| `SEC-chest-traversal` | Chest keys reject `..`, absolute paths, `~`, URL-encoded and double-encoded traversal, backslashes, symlink escape, and NUL bytes. Nothing is ever written or read outside `/data/chest/<node_id>/`. | **S1** |
| `SEC-chest-isolation` | A chest wire grants access to that chest only, not to sibling chests by id-guessing. | **S1** |
| `SEC-script-token-scope` | A script's token carries the SCRIPT's wires, not its caller's; a script cannot reach nodes only its caller is wired to (no confused deputy). | **S1** |
| `SEC-script-timeout` | `timeout_secs` is enforced; a runaway script is killed and reported, not left running. | S2 |
| `SEC-mcp-scope` | An MCP node is attached only to agents wired to it; MCP env/config never leaks to other agents. | **S1** |
| `SEC-engine-secret` | `WHEEL_ENGINE_SECRET` is never present in any agent's env, any log, or any API response. | **S1** |
| `SEC-ports-closed` | The container publishes no ports; the engine is reachable only on the docker network. | **S1** |

---

## 10. COMMS — §3c hardening

One ID per row of the §3c table, so we can prove each YOKE lesson was actually learned.

| ID | §3c # | Criterion |
|---|---|---|
| `COMMS-mcp-tools` | 1 | The built-in MCP server exposes exactly `msg, read, write, rm, ls, query, secret_get, run, ctx_clear, inbox, whoami, connections`; each is wire-gated identically to the CLI verb (a denied tool call fails like exit 3). *(M2)* |
| `COMMS-cli-warn` | 1 | `CLI-msg-argv-warn`. *(M1)* |
| `COMMS-inbox` | 2 | `MSG-inbox-*`. |
| `COMMS-sha` | 3 | `MSG-sha256` + `MSG-byte-exact`. |
| `COMMS-states` | 4 | `MSG-state-machine` + `MSG-state-event`; `--wait` / `--wait-consumed` block correctly. *(`--wait` M2)* |
| `COMMS-attribution` | 5 | `MSG-envelope-escape` + `MSG-envelope-forge` + `MSG-envelope-attrs`. |
| `COMMS-limits` | 6 | `MSG-limit-*` and `CLI-limits-clientside`. |
| `COMMS-ls-bare` | 7 | `CLI-ls-bare`. |
| `COMMS-fanout` | 8 | `wheel msg a,b,c` and `--all` create one row per recipient, one call; a denied recipient in the list fails that recipient without silently dropping the others. *(M2)* |
| `COMMS-threading` | 9 | `--reply-to <id>` sets envelope `reply_to`. *(M2)* |
| `COMMS-observability` | 10 | Web shows body, sha256, state, from/to, and the exact stdin transcript per agent. *(Web M2)* |
| `COMMS-no-truncate` | 11 | `MSG-no-truncate`. |
| `COMMS-single-writer` | 12 | `MSG-single-writer` + `MSG-no-midturn` + `MSG-priority-*` + `MSG-stdin-sole-path`; interrupt is `MSG-interrupt`. |

---

## 11. E2E — browser (Playwright)

| ID | Criterion |
|---|---|
| `E2E-landing` | Landing page renders, no console errors. |
| `E2E-signin` | Sign-in via Clerk test mode; unauthenticated `/app` redirects. |
| `E2E-project-create` | Create a project; it appears and reaches `running`. |
| `E2E-place-nodes` | Place an `agent` and a `ctx` node on the board; they persist across reload. |
| `E2E-wire` | Draw `ctx→agent`; an illegal wire is refused **in the UI** with a reason, not just by the API. |
| `E2E-inspector` | Inspector shows and edits config for each node type. |
| `E2E-start-agent` | Start the agent (fake harness); status goes `starting → running/idle`. |
| `E2E-chat` | Send a chat message; the reply appears in the log (the fake echoes, so the assertion is exact). |
| `E2E-injection-visible` | The ctx markdown is visible in the agent's log/transcript view, proving injection end-to-end through the UI. |
| `E2E-vault-masked` | Vault values are never rendered, even in DOM or network responses. **S1.** |
| `E2E-testids` | Every assertion uses a stable `data-testid`. New ones requested from Web via PM — never by scraping text. |

---

## 12. BACK / PERF — backends and scale

| ID | Criterion |
|---|---|
| `BACK-docker` | Full suite passes with `SANDBOX_BACKEND=docker`. |
| `BACK-process` | **The entire suite re-runs unchanged with `SANDBOX_BACKEND=process`** and passes. The suite is parameterised on this from day one, so M3 is a CI matrix flip, not a rewrite. *(M3)* |
| `BACK-parity` | Any behaviour that legitimately differs between backends is documented and asserted as a difference, not quietly tolerated. *(M3)* |
| `PERF-200-nodes` | A board of 200 nodes loads and renders; `GET /v1/board` stays within a documented budget. *(M3)* |
| `PERF-1000-msgs` | 1000 messages queue and drain in order with no loss, no duplication, no state skips. *(M3)* |
| `PERF-soak` | A 30-minute soak with agents messaging continuously: no fd/memory/zombie growth. *(M3)* |
| `PERF-msg-latency` | Message → stdin latency within the documented budget; one agent process sustains 10 sequential turns without leak or slowdown. |
| `PERF-check-budget` | `make check` stays under 3 minutes. |
| `GATE-coverage-rust` | §0b: `cargo llvm-cov --workspace --fail-under-lines 90` runs in `make check` and **fails** the check below 90%, per crate. |
| `GATE-coverage-web` | §0b: `vitest --coverage` with a `lines: 90` threshold, failing the check below the bar. |
| `GATE-adversary-review` | §0b: every merged milestone deliverable is handed to ADVERSARY as a `DONE:`; plans reviewed before M1 merges. |

---

## 13. Open questions

| ID | Question | My recommendation |
|---|---|---|
| `Q-HARNESS-CODEX` | Real `codex exec --json` event names are unverified; `fake-codex` is provisional. | Leave it. M1 is claude-only; SDK + QA pin it before M2. **PM/SDK agreed (A6).** |
| ~~`Q-INJ-ORDER`~~ | **RESOLVED** by PM: ctx blocks ordered by ctx node name, byte order. Now asserted as `INJ-multi-ctx`. | — |
| ~~`Q-MSG-ERROR-REDELIVER`~~ | **RESOLVED** by PM, as recommended: consumed exactly once, `error=true`, never redelivered. Now `MSG-poison-once`. | — |
| ~~`Q-TABLE-CEILING`~~ | **RESOLVED** by PM, as recommended: 300 s and 10,000 rows/keys. Now `CLI-ceiling-*`. | — |
| `Q-TOOL-ALLOWLIST` | §3d says v1 SSRF policy is deny-all-private with "project allowlist maybe later". Does anything legitimately need to call a private address in v1? | Keep v1 deny with no escape hatch. An allowlist is the feature most likely to be added carelessly later and re-open every `TOOL-ssrf-*` case; better to add it deliberately with its own tests. |
| ~~`Q-ENDPOINT-AUTH`~~ | **RESOLVED** by PM: `auth: {mode:"bearer", vault_ref}` in M2, requiring an `endpoint→vault read` wire. Now `ING-auth-bearer*`. | — |
| `Q-PRIORITY-FAIRNESS` | What stops a stream of user messages starving queued agent messages indefinitely (`MSG-priority-no-starve`)? | Drain at most N user messages before letting one queued message through, or timestamp-age escalation. Needs a rule from SDK before I can assert one. |

---

## 14. Traceability

- Wire matrix cells: `qa/fixtures/wire_matrix.json` (generated; `make check` fails on drift).
- Fake harness contract: `qa/harness/README.md`.
- Open bugs by ID: `qa/BUGS.md`.
- Suites: `qa/contract/` · `qa/integration/` · `qa/e2e/`.
