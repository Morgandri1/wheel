# Wheel — Test Plan & Acceptance Criteria

Owner: QA. Source of truth for *what "done" means*, derived line-by-line from
`docs/ARCHITECTURE.md`. If a criterion here contradicts the architecture doc, the architecture doc
wins and this file is the bug — tell me and I'll fix it.

**Every criterion has an ID.** Tests reference the ID; bug reports quote the ID; `qa/BUGS.md`
tracks failures by ID. If you're implementing something and want to know when it's finished, find
its ID here.

## 0. Conventions

**ID scheme:** `<AREA>-<thing>-<case>`, e.g. `WM-agent-ctx-read`, `API-auth-owner-404`.

**Areas:** `NODE` data model · `WM` wire matrix · `MSG` message delivery · `INJ` injection &
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
| `NODE-endpoint-path-collide` | Two endpoints with the same method+path → 409. |
| `NODE-script-lang` | `script`: `language ∈ {python,ts,js}`; `timeout_secs` defaults to 60, must be > 0 and ≤ a documented ceiling. |
| `NODE-mcp-transport` | `mcp`: `stdio` requires `command`; `http` requires `url`; supplying both/neither → 400. |
| `NODE-vault-writeonly` | `vault.config.keys` lists key NAMES only; values are never accepted here. |
| `NODE-schema-roundtrip` | Every node type round-trips create → `GET /v1/board` → update → read without field loss or type coercion. |

---

## 2. WM — wire matrix (§3), exhaustive

All **192** cells (8 node types × 8 × 3 wire types) are enumerated in
**`qa/fixtures/wire_matrix.json`**, generated from the contract by
`qa/tools/gen_wire_matrix.py`. **22 allow, 170 deny.** `make check` fails if the fixture drifts
from the generator, so the doc, the fixture and the tests can't disagree.

IDs are `WM-<from>-<to>-<type>`, e.g. `WM-agent-ctx-read` (allow),
`WM-table-agent-send` (deny).

| ID | Criterion |
|---|---|
| `WM-create-allow` | Each of the 22 allowed cells: `POST /v1/wires` → 201, wire appears in `GET /v1/board` on the FROM node's outgoing list. |
| `WM-create-deny` | Each of the 170 denied cells: `POST /v1/wires` → 4xx with a machine-readable reason; no wire row is created. |
| `WM-engine-deny` | Same 170, posted directly to the **engine** control plane, bypassing the API → rejected. Independent defence. |
| `WM-enforce-runtime` | For each allowed cell, the corresponding `wheel` CLI call succeeds; for each denied cell, it fails with **exit 3** and touches nothing. |
| `WM-write-implies-read` | A `write` wire to table/chest grants read too (`wheel table query` works with only `write` wired). |
| `WM-read-not-write` | A `read` wire does NOT grant write: INSERT via a read-only table wire → exit 3; `wheel chest put` → exit 3; `wheel write <ctx>` → exit 3. |
| `WM-no-wire-denied` | With no wire at all, every CLI verb against that node fails exit 3 — including read verbs, and including nodes that don't exist (same error, no existence oracle). |
| `WM-wire-revoked` | Deleting a wire revokes access for an **already-running** agent without restart; the next CLI call fails exit 3. |
| `WM-token-scope` | A node's token grants exactly its own wires: agent A's token used against agent B's wired nodes → exit 3. Token forgery / swapping is rejected. |
| `WM-self-wire` | A node wired to itself is rejected for every type (incl. `agent→agent send` to self). |
| `WM-dup-wire` | Creating an identical wire twice → 409 or idempotent 200, never two rows. |
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
| `MSG-error-turn` | `result.is_error=true` → agent `status=error`, `last_error` = `result.result`; the message is still marked consumed (not redelivered in a loop). | S2 |

---

## 4. INJ — ctx injection & ephemeral context (§3)

| ID | Criterion |
|---|---|
| `INJ-on-start` | ctx markdown from every `ctx→agent send` wire is present in the composed system prompt at start — asserted from the fake's **first event**, i.e. what the child actually received. |
| `INJ-multi-ctx` | Multiple wired ctx nodes are all injected, in a defined, stable order (document the order; assert it). |
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
| `ENG-restart-persist` | Board, messages, table data and chest blobs all survive an engine restart. |

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
| `ING-ratelimit` | Documented rate limit is enforced and returns 429. | S3 |
| `ING-header-filter` | Only the documented header subset is forwarded; `Authorization`/cookies are not leaked into the agent's prompt. | **S1** |

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
| `PERF-check-budget` | `make check` stays under 3 minutes. |

---

## 13. Open questions

| ID | Question | My recommendation |
|---|---|---|
| `Q-HARNESS-CODEX` | Real `codex exec --json` event names are unverified; `fake-codex` is provisional. | Leave it. M1 is claude-only; SDK + QA pin it before M2. **PM/SDK agreed (A6).** |
| `Q-INJ-ORDER` | In what order are multiple ctx nodes injected, and where does `system_prompt` sit relative to them? | Define it as: `system_prompt` first, then ctx nodes ordered by node name (stable, board-position-independent). Needs SDK confirmation to become `INJ-multi-ctx`. |
| `Q-MSG-ERROR-REDELIVER` | When a turn errors, is the message consumed or retried? | Consumed exactly once + `last_error`. Infinite redelivery of a poison message is worse than losing one. |
| `Q-TABLE-CEILING` | Documented ceiling for `script.timeout_secs` and max rows returned by `table query`? | 300s and 10k rows; both need to be in PROTOCOL.md so `CLI-limits-clientside` can enforce them. |
| `Q-ENDPOINT-AUTH` | Can an ingress endpoint require a shared secret? | Out of scope for M1/M2, but worth a line in the threat model — a public write path into a table is an obvious abuse target. |

---

## 14. Traceability

- Wire matrix cells: `qa/fixtures/wire_matrix.json` (generated; `make check` fails on drift).
- Fake harness contract: `qa/harness/README.md`.
- Open bugs by ID: `qa/BUGS.md`.
- Suites: `qa/contract/` · `qa/integration/` · `qa/e2e/`.
