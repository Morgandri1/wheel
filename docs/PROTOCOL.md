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

**The unix socket admits only its own uid and root.** In process mode the socket is the tenant boundary, so
it carries two locks: mode `0600` in a `0700` directory (who *may* connect) and a `SO_PEERCRED` check on every
accepted connection (who *did*). Anything else is closed immediately and logged with the peer's uid and pid.
Root is admitted because the host proxies API traffic through this socket and runs privileged in process mode.
The second lock exists because the first is a file permission, and file permissions are lost to umasks, chmods
and restored backups; verified in a container by deliberately making the socket `0666` in a `0777` directory
and confirming another uid is still refused.

**A dead child is reaped, and a start after a failure really starts.** The supervisor owns the process, so it
is the only thing that decides an agent is no longer running: when the child's pipes close, its slot is
cleared, anything written to it that never ran a turn goes back to `queued`, its node token is revoked, and the
status settles (`needs_auth` / `error` / `stopped` — an exit with nothing on stderr is `stopped`, because that
is also what a clean shutdown looks like). Each spawn carries a run id, so a dying child never settles a slot
that already holds its replacement. Before this, a slot left occupied made every later `start` a silent
no-op — `200 OK`, no process — which is exactly the path an operator walks when authenticating for the first
time.

### Vault nodes — secrets, and credentials per account

```
PUT    /v1/vault/:id/:key   {value}   → {key, stored: true}   write-only
DELETE /v1/vault/:id/:key             → 204
GET    /v1/vault/:id                  → {keys: [...]}         NAMES only
GET    /v1/cli/secret?addr=<vault>/<key>   → {node, key, value}   wire-gated, agents
GET    /v1/cli/secret/keys?node=<vault>    → {node, keys}         wire-gated, agents
```

Values are encrypted at rest with AES-256-GCM under the project's `WHEEL_VAULT_KEY`, and each
ciphertext is bound to its vault id and key name — a row copied to another key or another node fails
to decrypt rather than quietly becoming that other secret. Values never appear on `GET /v1/board`,
in a log line, or in a transcript.

A key whose name is one of `CLAUDE_CODE_OAUTH_TOKEN`, `ANTHROPIC_API_KEY`, `CODEX_API_KEY` is a
**credential**: it is exported into the child's environment at spawn, so an agent with a read wire to
a vault holding one is authenticated without anything being pasted into the UI. That is how one
project runs several accounts of the same provider — **one vault per account**, and an agent uses the
vault it is wired to. A vault-supplied credential wins over a pasted one: the vault is the thing the
operator can see and change on the board. `GET .../auth` then reports
`{authenticated: true, mode: "env", source: "<vault name>"}` — the name, never the value.

**Ambiguity is refused, never resolved.** An agent wired to two vaults that both define the same key
has no correct answer; resolving it would choose an account on the user's behalf and say nothing. It
is rejected in three places, because there are three ways to reach the same broken state:

| Where | Response |
|---|---|
| creating the second `agent → vault (read)` wire | `409 ambiguous_credential`, naming the key and both vaults |
| `PUT /v1/vault/:id/:key` adding a key another wired vault already supplies | `409 ambiguous_credential` |
| agent start | refused with the same reason — the only check guaranteed to run for a board restored from an export, or wired before this rule existed |

The rule covers **any** duplicate key, not only the three credential names: every vault key is
exported as an environment variable, so two vaults defining `FOO` is the same silent coin-flip. The
message says `ambiguous credential <KEY>` for a recognised credential and `ambiguous vault key <KEY>`
otherwise. Two *different* agents may of course use different vaults for the same key — the check is
per agent, or the multi-account feature would forbid itself.

Vaults are **read-only to agents** (§3e): `wheel secret get` and `wheel secret list` work, `set` does
not. An agent that could write a vault could rewrite the credential another agent runs as.

Secrets an agent can read are redacted from its log and transcript lines. That is
**accidental-echo protection, not a containment boundary** — an agent that can read a value can
transform it past any matcher. It exists because children print their own environment constantly.

### `run_on_startup` — parked, not running

An agent configured `run_on_startup` comes up **`parked`** at engine boot: logically on, no process (§2 —
`run_on_startup` starts them parked). It spawns when something is addressed to it, resuming its session
transparently. A board of twenty such agents therefore costs zero idle processes, which is the whole point;
the trade is explicit — **an agent that is never messaged never spawns.** Work queued while the engine was
down is not stranded by this: boot resumes exactly those parked agents that already have a queued message.

Every enqueue path goes through the supervisor's `deliver`, which resumes a parked agent before pumping, so
"queued to a parked agent" is never a message that waits forever.

### `ephemeral_context`

When a turn completes on an agent configured `ephemeral_context`, the engine discards the session and starts a
new one — system prompt and every wired `ctx` node re-injected — and only then drains the next queued message.
The stored session id is cleared *before* the restart, so the new child does not `--resume` the context that
was just discarded. `POST /v1/agents/:id/clear` (and `wheel ctx clear`) take the same path on demand; the two
are the same code, so they cannot drift apart.

`GET .../log` `stream` filter accepts `stdout | stderr | engine | transcript`. `transcript` is the exact bytes
the engine wrote to the child's stdin (§3c#10), exposed on this same route and as ordinary `log` events so the
UI needs no second subscription (agreed with Web, M2). `seq` is monotonic per agent and is the resume cursor.

### Auth (per agent node)

| Route | Body → Response | M |
|---|---|---|
| `POST /v1/agents/:id/auth/begin` | → `AuthBegin {mode, url, instructions, session}` (claude only) | **M1** |
| `POST /v1/agents/:id/auth/complete` | `{api_key?}` \| `{code?}` → `AuthStatus` | api_key **M1** · code M2 |
| `GET /v1/agents/:id/auth` | → `AuthStatus {authenticated, mode, account?}` | **M1** |
| `DELETE /v1/agents/:id/auth` | → `204`, forgets the stored credential | **M1** |

**`mode` is `CredentialKind`**: `"api_key"` · `"oauth_token"` · `"oauth_session"` · `null` when nothing is
stored. It says what kind of credential the node holds, which is a different axis from `AuthMode` on
`auth/begin` (how one is *obtained*) — the two share a field name and nothing else.

#### Paste-code OAuth (`claude`)

Two calls with a **live child process between them**, which is the only real complexity in this flow:

1. `POST auth/begin` spawns `claude auth login --claudeai` with the node's own `CLAUDE_CONFIG_DIR`/`HOME`, reads
   the authorize URL off its stdout and returns `{mode:"paste_code", url, instructions, session}`. The child is
   left alive, blocked reading stdin.
2. The user opens the URL, signs in, and the Anthropic-hosted callback shows them a code. `POST auth/complete
   {code, session?}` writes it to that child's stdin and waits for it to exit.

The engine parses the URL by looking for `https://`, not for the sentence around it — the CLI's wording
("If the browser didn't open, visit: ") is cosmetic and has no contract behind it. Verified against the real
bytes of `claude 2.1.261`, which are pinned in a test.

`session` is optional but recommended: it stops a stale browser tab completing a login the user already
restarted. A second `auth/begin` kills the first child rather than leaking a process per retry, and a login
that is abandoned is evicted after **15 minutes**.

| Outcome | Response |
|---|---|
| success | `200 AuthStatus {authenticated:true, mode:"oauth_session"}` |
| no login in flight, expired, or superseded `session` | `409 {"error":{"code":"expired"}}` |
| the CLI rejected the code | `400` carrying **the CLI's own reason**, not a generic failure |
| the CLI never printed a URL, or never answered | `502` / `504` |
| the CLI exited 0 but wrote no credentials | `502` — success here would leave an agent that looks signed in and fails on its first turn |

`codex` signs in by device code, which is a poll rather than a submit; `auth/begin` on a codex node returns
`400` saying so rather than a paste-code envelope nothing can satisfy.

**Authenticating resumes the agent.** Saving a credential on a `needs_auth` agent moves it to `parked` and
delivers anything queued immediately — no restart, no second call. With an empty queue it stays parked and
costs nothing. The agent was started by someone who wanted it running, and a stuck queue behind a solved
problem is not a state worth making an operator clear by hand.

`auth/complete {api_key}` carries **either** a provider API key **or** the long-lived OAuth token from
`claude setup-token`, and the engine tells them apart by prefix rather than making the caller declare which it
has:

| Token | Kind | Env var handed to the child |
|---|---|---|
| `sk-ant-oat…` | `oauth_token` | `CLAUDE_CODE_OAUTH_TOKEN` |
| anything else on a claude node (`sk-ant-api…`, a gateway key) | `api_key` | `ANTHROPIC_API_KEY` |
| anything on a codex node | `api_key` | `CODEX_API_KEY` |

Exactly one variable is ever set — exporting both would leave the winner to the harness's own precedence.
The kind is re-derived from the stored token on every read, never recorded in a second file that could drift
out of sync with it. A token beginning `sk-ant-` sent to a **codex** node is refused with `400`: it
authenticates nothing there, and accepting it buys a node that starts, looks healthy, and fails on its first
turn. Subscription accounts with no API key at all are the reason this path exists — `claude setup-token` is
their only headless credential, and sent as `ANTHROPIC_API_KEY` it is rejected in a way that reads as bad
credentials rather than a mis-addressed envelope.

Credentials live per node under `<data>/creds/<node_id>/`; each child gets its own `CLAUDE_CONFIG_DIR` /
`CODEX_HOME`, which is what lets two agent nodes in one sandbox be two different accounts (verified in a
container: two config dirs produce two independent `0600 .claude.json` trees).

The two harnesses need **opposite** flows, which is why `AuthMode` keeps them distinct:

| | `claude` | `codex` |
|---|---|---|
| Mode | `paste_code` — a **submit** | `device_code` — a **poll** |
| Who makes the code | the browser | the CLI |
| `auth/begin` | spawn `claude auth login --claudeai` on pipes, read the authorize URL off stdout, keep the child alive (TTL 15 min) | run `codex login --device-auth`, return `url` + `user_code` |
| `auth/complete` | write `<code>#<state>\n` to that child's stdin | returns current status; the engine is already polling |
| Pasted credential | `ANTHROPIC_API_KEY` or `CLAUDE_CODE_OAUTH_TOKEN`, routed by prefix (above) | **`CODEX_API_KEY`** — *not* `OPENAI_API_KEY`, which `codex doctor` reports as fine but which is not in the auth chain |
| Safe probe | `claude auth status --json` (`loggedIn`, `authMethod`) | `codex login status` |
| Unsafe probe | — | `codex exec` — proceeds unauthenticated and dies later on a runtime 401 |
| Keyring | none on Linux; plain `0600` file | `CODEX_HOME` does **not** isolate the OS keyring — each node's `config.toml` must set `cli_auth_credentials_store = "file"` |

`claude auth login` needs **no reachable localhost**: the redirect URI is Anthropic-hosted
(`platform.claude.com/oauth/code/callback`), the browser displays the code, and the container never receives a
callback. Verified live: over a pipe with no TTY the CLI prints the URL and consumes a piped code (a fake one
produced a real `400` from the token exchange, proving the mechanism rather than a hang).

### Distinguishing `needs_auth` from misconfiguration — verified

`bypassPermissions`-as-root and not-being-logged-in both exit 1, so the exit code alone is useless. The
**stream** discriminates, and this is what the supervisor keys on:

| Observation | Meaning |
|---|---|
| a `system`/`init` line on stdout, then failure | the process started fine — auth or runtime problem |
| **no stdout at all** + exit 1 | misconfiguration (the root trap), **not** `needs_auth` |
| `claude auth status --json` → `loggedIn:false` | authoritative `needs_auth` |

`needs_auth` is only ever set from the explicit probe or an authenticated-failure signal — never inferred from
an exit code.

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
