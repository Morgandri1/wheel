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

**`/v1/cli/*` is the surface an untrusted process talks to**, so it is the one most worth having written
down. Every route below resolves the caller from its token, checks the wire, and answers `403 wire_denied`
(exit 3 at the CLI) rather than pretending the target does not exist. `ctx/clear` takes no target because
an agent may only clear its OWN context: one that could clear a peer's could erase what that peer was told
without leaving a trace in either transcript.
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
| `conflict`/`expired` | 409 | `auth/complete` with no login in flight, an expired one, or a superseded `session`. |
| `ambiguous_credential` | 409 | Two vaults an agent can read supply the same key. Names both vaults and the key. |
| `config` | 503 | The engine cannot do this because of how it was STARTED (e.g. no `WHEEL_VAULT_KEY`). Names the variable. Not the caller's fault and not retryable without an operator. |
| `agent_running` | 409 | Rename of an agent that is running or starting. Stop or park it first. |
| `internal` | 500 | A fault in the engine. Anything else is a code above; a 500 is a bug worth reporting. |

**Every error response carries a body**, always the same shape — there is no code that returns a bare
status. Clients can render `error.message` verbatim for every one of them:

```json
{ "error": { "code": "wire_denied", "message": "no wire from worker to secrets (need: read)" } }
```

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
GET    /v1/cli/whoami                      → {name, id, type, position, wires}
GET    /v1/cli/connections                 → {wires: [{peer, type, outgoing, semantics}]}
GET    /v1/cli/list                        → {agents: [{name, status, session_id, hosted_on}]}
GET    /v1/cli/ls[?node=<n>&prefix=<p>]    → {keyspaces} with no node, else {keys}
GET    /v1/cli/read?addr=<node>[/<row>]    → ctx markdown / table row / chest blob
POST   /v1/cli/write   {addr, value}       → upsert; ctx replace, table row, chest blob
POST   /v1/cli/rm      {addr}              → {node, row, removed}
POST   /v1/cli/query   {table, sql}        → {rows}   read-only, one table
POST   /v1/cli/msg     {to, body, reply_to?} → {id, sha256, bytes, state}
GET    /v1/cli/inbox[?id=<message id>]     → {messages} or one message, verbatim
POST   /v1/cli/ctx/clear                   → {node, cleared, status}   own context only
GET    /v1/cli/tool?node=<tool>            → {tool, operations}  agent-fill fields only
POST   /v1/cli/tool   {node, op, args, curl?} → the call result, or the masked curl
GET    /v1/cli/mcp/tools                   → {tools} the MCP tool list for this node
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
| `POST /v1/agents/:id/auth/complete` | `{api_key?}` \| `{setup_token?}` \| `{code?}`, each with optional `save_to_vault` → `AuthStatus` | api_key **M1** · code/setup_token **M2** |
| `GET /v1/agents/:id/auth` | → `AuthStatus {authenticated, mode, account?}` | **M1** |
| `DELETE /v1/agents/:id/auth` | → `204`, forgets the stored credential | **M1** |

**`expires_at`** (RFC3339, omitted when absent) says when the stored credential stops working. It is
reported for `mode: "env"` (from the vault row the credential was stored on) and for
`mode: "oauth_session"` (from the harness's own store, which is the same file the child reads).

**Absent means durable OR unknown — those are not the same thing, and the engine will not guess.** A UI
must not render a deadline when the field is missing. When it IS present and in the past, the agent will
not start: `POST /v1/agents/:id/start` returns `needs_auth` with a `last_error` naming the vault and the
durable fix, and no child process is spawned — the operator sees the one status they can act on instead
of a harness failing on its first request.

**`mode` is `CredentialKind`**: `"api_key"` · `"oauth_token"` · `"oauth_session"` · `"env"` · `null` when
nothing is stored. It says what kind of credential the node holds, which is a different axis from `AuthMode` on
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
restarted. A second `auth/begin` kills the first child rather than leaking a process per retry.

A login that is abandoned — the common case, a user who signs in halfway and closes the tab — is collected
after **15 minutes** by a timer armed when it starts, so nothing has to call back in for that child to be
reaped. One timer per login rather than a sweep: an engine where nobody is signing in must not wake up to
discover that.

| Outcome | Response |
|---|---|
| success | `200 AuthStatus {authenticated:true, mode:"oauth_session"}` |
| no login in flight, expired, or superseded `session` | `409 {"error":{"code":"expired"}}` |
| the CLI rejected the code | `400` carrying **the CLI's own reason**, not a generic failure |
| the CLI never printed a URL, or never answered | `502` / `504` |
| the CLI exited 0 but wrote no credentials | `502` — success here would leave an agent that looks signed in and fails on its first turn |

`codex` signs in by device code, which is a poll rather than a submit; `auth/begin` on a codex node returns
`400` saying so rather than a paste-code envelope nothing can satisfy.

#### `setup_token` — the credential to prefer for a shared board

`auth/complete {setup_token}` takes a long-lived token from `claude setup-token`. It is a separate field
from `api_key` rather than a second spelling of it, because it **asserts durability**: a credential that
is not a `sk-ant-oat…` setup-token is refused here with a message pointing at `api_key` instead. The whole
reason to reach for this field is the promise that the credential will not expire underneath a board of
agents, and silently accepting a session token would break that promise where nobody would see it.

Stored as `CLAUDE_CODE_OAUTH_TOKEN`; `mode` comes back as `"oauth_token"`. Claude only — a codex node
takes `api_key` (`CODEX_API_KEY`).

**Operator flow for a board of N agents:** run `claude setup-token` once, then
`POST auth/complete {setup_token, save_to_vault: "<vault>"}` against any one agent that has a read wire to
that vault. Every agent wired to it authenticates with `mode: "env"` on its next start. No expiry, no
refresh, one text box.

`save_to_vault` works with all three credential fields.

#### Handing the credential to the rest of the board (`save_to_vault`)

`auth/complete` accepts an optional `save_to_vault: "<vault name>"`. After a successful login the engine
reads the credential out of that node's own store and writes it to the named vault as
**`CLAUDE_CODE_OAUTH_TOKEN`**, through the same encrypted, write-only path as `PUT /v1/vault/:id/:key` —
so the ambiguity rule applies and the value never appears on `/v1/board`, in a log, or in a transcript.
Every agent with a read wire to that vault then authenticates with `mode: "env"` on its next start, which
is how one browser round-trip authenticates a board of six agents.

**A credential that expires is REFUSED for a vault other agents read.** The spawn gate checks the vault's
expiry per agent, so one lapsed session credential in a vault with N readers stops all N at once — and the
person who saw the warning is not the person stranded. Response is `409 shared_expiry`, naming every peer
that would be affected:

```jsonc
{ "error": { "code": "shared_expiry", "message":
  "this credential expires, and researcher, reviewer also read anthropic: when it lapses they all stop at
   once. Use a `claude setup-token` credential, which does not expire, or resend with allow_shared_expiry
   to accept that." } }
```

`allow_shared_expiry: true` proceeds anyway — it exists because an operator with no CLI cannot run
`claude setup-token`, and for them paste-code + `save_to_vault` is the only way to authenticate a board.
The point is to make it a decision rather than a surprise. The success response then carries `shared_with`
listing the peers. A durable credential (`sk-ant-oat…`) never triggers this, and a provider API key has no
expiry to begin with.

The agent **must already have a `read` wire to the vault**; without one this is `403 wire_denied`. The
wire is the capability here as everywhere else: an agent may not write its credential into a keyspace it
has no relationship with.

**Which credential ends up in the vault, and the catch.** The engine takes the access token from the
harness's own credential store. A subscription login stores a **session** credential that the CLI
refreshes in place, so a copy of it in a vault works now and stops working when it expires — silently,
for every agent reading that vault. The response therefore reports what was stored:

```jsonc
{ "authenticated": true, "mode": "oauth_session",
  "vault": { "name": "anthropic", "key": "CLAUDE_CODE_OAUTH_TOKEN", "stored": true,
             "expires_at": 1799999999000,
             "warning": "this is a session credential and will expire; for a durable one, run
                         `claude setup-token` and submit that token as api_key instead" } }
```

`warning` is present whenever what was found is not durable. **The durable path is
`claude setup-token`**, which mints a long-lived `sk-ant-oat…` with no expiry; submit that through
`auth/complete {api_key}` (the engine routes it to `CLAUDE_CODE_OAUTH_TOKEN` by prefix) or store it in the
vault directly with `PUT /v1/vault/:id/CLAUDE_CODE_OAUTH_TOKEN`. Prefer it for anything shared.

If the CLI reorganises its credential store and the engine cannot find a token, this is `502` naming the
file and pointing at `setup-token` — never a success that vaulted the wrong string.



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
| `GET /v1/tables/:id/rows?limit&offset` | → `{node, columns, rows: object[], total, limit, offset}` | **M2** |
| `POST /v1/tables/:id/query` | `{sql}` → `{node, rows}`. Read-only, 5s timeout. | **M2** |
| `GET /v1/chests/:id/ls?prefix` | → `{entries:[{key,bytes,modified_at}]}` | M2 |
| `GET /v1/chests/:id/blob?key` | → raw bytes | M2 |
| `PUT /v1/chests/:id/blob?key` | raw body → `204` | M2 |

#### Table nodes

A table node **is** its sqlite table: `t_<name>`, with an implicit `key TEXT PRIMARY KEY` plus the configured
columns. The table is created, renamed and dropped with the node itself (in `db::board`, not in the route), so a
table node always has storage and a rename never orphans rows from their address. A table node's name must
already be a sqlite identifier — a node name may contain `-`, an identifier may not — so creating `my-notes` as a
table fails with a message naming `_` as the fix, rather than silently mangling the name.

| Address | Wire | Behaviour |
|---|---|---|
| `read <t>` | read | all rows, paged (`limit`/`offset`, ceiling 10,000) |
| `read <t>/<row>` | read | one row as JSON, `404` if absent |
| `write <t>/<row>` | write | upsert by key. The body is a JSON object of column values |
| `rm <t>/<row>` | write | → `{removed: bool}` |
| `ls <t> [prefix]` | read | row keys, ordered; the prefix is matched **literally** (`%` and `_` are escaped) |
| `query <t> "<SELECT …>"` | read | read-only SQL, scoped to that one table |

A write **replaces** the row: a column the caller omits becomes `NULL` rather than keeping its previous value, so
writing the same key twice with different fields cannot leave a hybrid of the two behind. A column the caller
invents is a `400` listing the real columns — never a silent no-op, which would report success and write nothing.
`key` may not appear in the body: it comes from the address, and accepting both would let them disagree. Values
are checked against the column type rather than coerced (`{"count": "three"}` is refused); `null` is always
allowed. `blob` columns are base64; `json` columns round-trip as the value written, not as a string containing it.

**`wheel query` is the only place an agent's own string reaches sqlite**, so it is layered rather than clever.
Each of these alone would be an argument; together they are the boundary:

1. A **separate connection**, opened `READ_ONLY`, so nothing can touch the engine's own connection or its
   transactions — and a slow query cannot stall message delivery.
2. An **authorizer that denies by default** and allows reading exactly one table. The allow rule is
   **case-insensitive**, because sqlite identifiers are: a case-sensitive rule would be bypassed by
   `SELECT * FROM T_SECRETS`. `sqlite_master` is denied too — which tables exist is itself information the
   querying node may not be wired to.
3. **One statement only**, because `prepare` stops at the first and silently discards the rest.
4. A **5 s deadline**, because `WITH RECURSIVE` can spin without touching a table, so the authorizer would never
   be consulted again.
5. **Size caps**: 8 MiB per value (`SELECT randomblob(1e9)` is one row, so the row ceiling never sees it) and
   16 MiB per response. The 10,000-row ceiling is a fetch counter, not a truncation of something already in
   memory, so a cartesian self-join costs 10,000 rows rather than all of them.

`ATTACH`, `DETACH`, `PRAGMA`, every write verb, and `load_extension` are all rejected. Refusals name the object
sqlite blocked and add why.

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

## Embedding the engine (M1.7)

`wheel-engine` is a library with a thin binary, so `wheeld` can run an engine
in-process instead of shelling out to one:

```rust
use wheel_engine::{serve, Config};

let cfg = Config::from_env()?;          // or build one directly
runtime.block_on(serve(cfg))?;          // runs until SIGTERM / shutdown
```

`serve` does not own a runtime and does not call `std::process::exit`, so an
embedder can host it alongside other work. The binary is a wrapper around this
exact call — there is one implementation of what an engine is, and no second
copy to drift from it.

### Shared-uid mode ("laptop mode")

The supported posture is **one unix uid per project** (§2, §5b): the data dir is
0700 to that uid and the engine socket is openable only by it. A local user
without the privileges to `setuid` can opt out of that:

```
WHEEL_ALLOW_SHARED_UID=1
```

It is an **opt-in, never a fallback**. A production host that silently dropped
to a shared uid after a permissions change would keep serving, keep looking
healthy, and have no tenant isolation — and nothing in its logs would be
alarming enough to catch it. So `setuid` failing is an error, not a downgrade.

With it set, the engine logs `SHARED_UID_WARNING` at **warn** on every boot,
naming what is gone rather than saying "reduced isolation":

> every project runs as THIS user, so the per-project boundary does not exist.
> Any agent can read any project's data directory, open any project's engine
> socket, and read any other child's environment. Vault values are protected
> only by the encryption key, which lives in that same environment.

**The host must refuse to start a second project in this mode.** One project as
one user is a convenience; two projects as one user is a tenancy boundary that
does not exist while claiming to. `wheel_core::UidIsolation::from_env()` is the
single reader of that variable, so the host and the engine cannot disagree about
which mode they are in.

## Tool nodes (§3d)

The engine is the ONLY parser. Web sends the raw document and renders what comes back, so a spec cannot
import differently in the preview than in the node.

### `POST /v1/tools/import` — preview, creates nothing

Request `{ "raw": "<document>", "format"?: "openapi"|"swagger2"|"postman"|"insomnia" }`. `format` is
optional; every format announces itself and detection is preferred.

```jsonc
{ "format": "openapi",
  "base_url": "https://api.petstore.example/v1",
  "operations": [ {
      "id": "listPets",              // charset-safe, unique in the node; becomes half an MCP tool name
      "method": "GET",
      "path": "/pets/{petId}",       // Postman's `:petId` is normalised to this shape
      "summary": "List all pets",
      "enabled": true,
      "params": [ {
          "name": "limit",
          "location": "query",       // header | path | query | cookie | body
          "required": false,
          "description": "how many",
          "schema": { "type": "integer" },
          "fill": { "mode": "agent" }   // ALWAYS agent on import; see Fills
      } ] } ] }
```

Errors: `400 invalid` — `"could not tell what kind of document this is…"`, `"this is neither valid JSON
nor valid YAML"`, `"no operations found in this document"`. A document with one unusable operation out of
forty imports the other thirty-nine rather than failing.

### Body fields are FLAT NAMES, not JSON pointers

**This deviates from the wording in ARCHITECTURE.md §3** (`fills: { "<json-pointer or dotted path>": Fill }`)
and Web should build against what is here, which is what the engine and `wheel-core` actually do.

A JSON request body contributes one param per **top-level property**, with `location: "body"` and `name`
set to that property name. There are no pointers, no dotted paths, and no `body` object in the config:

```jsonc
// requestBody schema { "properties": { "name": {...}, "address": { "type": "object", ... } } }
"params": [
  { "name": "name",    "location": "body", "schema": { "type": "string" }, "fill": {"mode":"agent"} },
  { "name": "address", "location": "body", "schema": { "type": "object" }, "fill": {"mode":"agent"} }
]
```

**Nested objects are kept whole.** `address` is one field whose value is an object; it does not become
`/address/city` and `/address/street`. An agent filling one object is clearer than filling three strings,
and the schema stays honest about the shape the API wants. A body that is not an object (an array, a
string) becomes a single field named `body`.

**Cookie values are percent-encoded**, like path and query values. `;` is legal in a header value so
nothing downstream rejects it, and an unencoded cookie value of `x; admin=true` would be two more cookies
the caller never granted.

In `args` they are therefore addressed by plain name, and a body field keeps the caller's own JSON type —
`{"address": {"city": "Berlin"}}` is sent as an object, not as a string. Header, path, query and cookie
values are always sent as text.

### `POST /v1/tools/:id/import` — re-import into an existing node

Diffs by `method` + `path`. **Fills survive, and so does `enabled` and the operation's `id`.** Re-importing
must never hand a field back to the agent that an operator pinned to a vault, or a routine spec refresh
silently becomes "the agent can now set the API key". A field that disappears and later returns is matched
by name and gets its pin back.

```jsonc
{ "operations": [ ... ],        // the merged set, as above
  "added":   ["createPet"],     // ids present in the spec and not in the node
  "removed": ["deletePet"],     // ids in the node that the spec no longer has —
                                // REPORTED, not deleted: the operator decides
  "unpinned": [] }              // pins this import would drop (see below)
```

Operations are matched with method and path compared **case-insensitively, ignoring a trailing slash**, and
params by **location + name** (case-insensitively). `id` in the path and `id` in the query are different
fields; an upstream normalising `/pets` to `/pets/` is not a new operation.

**A re-import that would drop a pin is REFUSED.** A renamed parameter has nowhere to put the operator's
`vault`/`static` fill, so the replacement would default to `agent` — turning a credential slot the agent
must never see into one it controls, with no add/remove signal because the operation itself still matched.

```jsonc
{ "error": { "code": "would_unpin", "message":
  "this spec no longer has getData.Authorization (vault), so a field pinned to the board would become
   agent-fillable. Re-pin on the new field names, or resend with allow_unpin to accept that." } }
```

`allow_unpin: true` proceeds, and the response's `unpinned` lists every dropped pin as
`{ "op", "param", "was": "vault"|"static" }`.

### `GET /v1/tools/:id/ops` — exactly what an agent sees

The same projection the CLI and (later) the MCP input schema are built from, so the UI's "what can the
agent do" and the agent's own view cannot drift. Disabled operations are absent. **Only `agent`-mode
fields appear**, and no `fill` block is included at all — a static value or a vault ref is not shown here,
because this is the agent's view.

```jsonc
{ "tool": "petstore",
  "operations": [ {
      "id": "listPets",
      "name": "petstore__listPets",     // the MCP tool name
      "method": "GET",
      "path": "/pets",
      "summary": "List all pets",
      "input_schema": {                  // JSON Schema of the agent's fields only
        "type": "object",
        "properties": { "limit": { "type": "integer", "description": "how many" } },
        "required": []
      } } ] }
```

### `POST /v1/tools/:id/call`

Request `{ "op": "listPets", "args": { ... }, "dry_run"?: false }`.

```jsonc
{ "status": 200, "headers": { "content-type": "application/json" },
  "body": { ... },              // parsed JSON, or a string when it is not JSON
  "duration_ms": 143, "bytes": 2048 }
```

`dry_run: true` sends nothing and returns the equivalent command instead, with **every static and vault
value masked** — in headers, cookies, body and the URL, in both its raw and percent-encoded spellings (a
secret placed in a query or path fill is stored encoded, so masking only the raw form missed every base64
credential):

```jsonc
{ "curl": "curl -X POST -H 'Authorization: <redacted>' -d '{\"text\":\"hi\"}' 'https://api.example.com/messages/g'" }
```

The agent's `/v1/cli` equivalents are `GET /v1/cli/tool?node=<tool>` and
`POST /v1/cli/tool {node, op, args, curl?}`, both gated on a `read` wire to the tool node.

### Refusals

| Situation | Code | HTTP | Message |
|---|---|---|---|
| Argument names a `static`/`vault`/`hidden` field | `invalid` | 400 | `"Authorization" is set by the board (a vault), not by the caller` |
| Argument names no field at all | `invalid` | 400 | `"admin" is not a field of operation listPets` |
| A required agent field is missing | `invalid` | 400 | `field "room" is required` |
| A `{placeholder}` left unfilled | `invalid` | 400 | `path parameter "room" was not supplied` |
| Operation is disabled | `invalid` | 400 | `operation deletePet is disabled` |
| No such operation | `not_found` | 404 | `no operation "nope" on petstore` |
| A `vault` fill whose vault the tool has no wire to | `wire_denied` | 403 | `no wire from petstore to creds (need: read) — wire the tool to the vault` |
| A `vault` fill naming a missing key | `not_found` | 404 | `creds has no key "API_KEY"` |
| Engine started without `WHEEL_VAULT_KEY` | `config` | 503 | names the variable |
| The upstream call failed, was denied by SSRF policy, timed out, or exceeded 5 MiB | `tool_error` | 502 | the reason |

There is no "ambiguous pointer" refusal, because there are no pointers — a duplicate body property is
impossible in JSON Schema, and two params with the same name are prevented at config validation
(`duplicate tool operation id` / a param list is per-operation).

**A field the board owns is REFUSED when an agent names it, never ignored.** Ignoring it would let an
agent believe it had set an authorization header the operator actually controls.

### Table node names

A `table` node's name becomes the sqlite table `t_<name>`, so it must already BE an identifier:
**`^[a-z][a-z0-9_]{0,62}$`** — stricter than the name rule every other node type follows, which permits `-`
and a leading digit. `table-1` is a legal node name and an illegal table node.

It is **refused, never rewritten**. Silently turning `table-1` into `table_1` would put the node at an
address the operator did not choose, and every `wheel read table-1` afterwards would fail for a reason
nothing explains. The error names the fix. Web should validate the same rule in the inspector so the
refusal arrives while the operator is still typing.

### Import limits

`raw` is capped at **2 MiB**. Separately, **YAML anchors and aliases are refused** (`400 invalid`, naming
the token): libyaml expands them with no limit and exposes no option to bound it, and a few hundred bytes
of aliases expand to ~10^9 nodes. `$ref` is the idiomatic mechanism and is depth-bounded instead. The size
cap is not the defence here — a billion-laughs document is tiny — so the two limits are separate on
purpose. JSON documents are unaffected.

### SSRF policy on every call (§3d rule 4)

`base_url` is checked at config time; every CALL re-checks, because a redirect is a destination nobody
named. Literal and suffix denials first (no DNS), then the host is resolved ONCE and the connection pinned
to the validated address — a name that answers differently a moment later cannot become a different
destination. **Every** resolved address must be public, not just the one chosen. Redirects are followed
manually, re-checked per hop, max 3, and the body is sent on the FIRST HOP ONLY so credentials do not
follow a redirect off-origin. 30s timeout, 5 MiB response ceiling.

## MCP: the board as tools (§3c #1)

The CLI is for scripts and humans. **MCP is what an LLM should use**, because a tool call is structured all
the way down — a body passed as argv goes through a shell first, where backticks and `$(…)` are substituted
before `wheel` ever sees it, and what arrives is silently not what was sent.

Every agent is started with an MCP config written to its run directory:

```jsonc
{ "mcpServers": { "wheel": {
    "type": "stdio", "command": "wheel", "args": ["mcp-serve"],
    "env": { "WHEEL_TOKEN_FILE": "/data/run/<node id>/token" } } } }
```

The token is passed as a **file path**, never a value — argv is world-readable across uids (§5b).

`wheel mcp-serve` speaks JSON-RPC 2.0 over stdio: `initialize`, `ping`, `tools/list`, `tools/call`. A
notification (no `id`) is never answered. The tool LIST comes from `GET /v1/cli/mcp/tools` rather than being
compiled in, so it reflects the caller's **current** wires — a tool node wired after the agent started
appears without a restart.

**Built-in tools**: `msg` · `read` · `write` · `rm` · `ls` · `query` · `secret_get` · `inbox` · `whoami` ·
`connections` · `ctx_clear`. Each maps to the `/v1/cli/*` route that already implements it, so there is one
implementation of what a tool does and nothing to drift.

`run` is deliberately **absent** until script nodes exist (M2). A tool whose route returns 404 teaches a
model that the board is unreliable, and it stops trying things that would have worked.

**Tool-node operations** appear as `<tool>__<op>` (§3d rule 7), with only the agent-fill fields in the input
schema — the same projection `GET /v1/tools/:id/ops` returns, so what the model is offered and what the
engine accepts are the same list. Only the FIRST `__` splits node from operation.

Descriptions name the nodes the caller can actually reach (`"Send a message to another agent: reviewer,
editor"`), which is the difference between a model guessing an address and knowing one. An agent wired to
nothing is told so plainly rather than shown a dangling list.

**A denial is a tool error, not a protocol error.** A wire refusal comes back as a successful JSON-RPC
response with `isError: true` and the engine's own message, so the model reconsiders and tries something
else. Reported as a protocol error it would tell the harness the server is broken, and it would stop asking.
