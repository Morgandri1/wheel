# Wheel — Threat Model (v1, ADVERSARY)

Scope: contract v1.1 (§3c comms hardening, §3d tool nodes, §4b host, §5b Railway process backend).
Wheel runs untrusted, LLM-driven code (agents, scripts, MCP servers, inbound HTTP) inside per-user
sandboxes, orchestrated by a stateless API + a single privileged host, multi-tenant on ONE kernel in
prod. Design axiom: **every layer is hostile to every other layer.**

## 1. Assets (what an attacker wants)
| # | Asset | Where it lives | Impact if lost |
|---|-------|----------------|----------------|
| A1 | Clerk session JWT | browser → API headers | Full account takeover of a user |
| A2 | `WHEEL_ENGINE_SECRET` (per project) | Postgres (enc), host runtime, engine env | Full control of a project's engine/agents |
| A3 | `WHEEL_HOST_SECRET` | host env only | Control of ALL tenants' sandboxes |
| A4 | `WHEEL_VAULT_KEY` (per project) | Postgres (enc), engine env | Decrypt all of a project's vault secrets |
| A5 | Vault values (API keys, bearer tokens) | sqlite ciphertext, agent env at spawn | 3rd-party account compromise; lateral movement |
| A6 | Cross-tenant data (/data/projects/<id>, wheel.db) | host disk | Confidentiality breach across users |
| A7 | Host docker socket / setuid ability | host process | Root-equiv on the sandbox machine → all tenants |
| A8 | In-agent creds (claude/codex OAuth, API keys) | child creds dir, /proc, env | Impersonate user's LLM accounts, spend |
| A9 | API master key, Postgres creds | API env, Railway private net | Decrypt all project secrets at rest |

## 2. Actors (capabilities, from weakest to strongest)
- **AN — Anonymous internet:** reaches `wheel.dev`, `api.wheel.dev/healthz`, `api.wheel.dev/p/<id>/*`
  (public ingress). No token.
- **AU — Authenticated other user:** valid Clerk JWT for their OWN account; wants another tenant's
  project/data (A1,A6 of victim).
- **PA — Prompt-injected / compromised agent:** an LLM child inside a sandbox. Can run any `wheel`
  CLI/MCP call, spawn scripts, read its own env/fs, make outbound HTTP via tool nodes. Untrusted by
  assumption — attacker controls its instructions via ctx markdown, inbound messages, or ingress bodies.
- **MS — Malicious script / MCP node:** operator-authored or agent-triggered code executing inside the
  sandbox with a node-scoped token; MCP `command` can exec arbitrary binaries as the engine uid.
- **MI — Malicious ingress caller:** anonymous HTTP hitting an endpoint node; controls method/path/
  headers/body delivered into an agent's turn.
- **MT — Malicious tenant (owner of project B):** combines AU + PA + MS against neighbours on the
  shared host (process backend). Highest-value adversary.

## 3. Trust boundaries + attack trees (ranked impact×likelihood)

### TB1 browser ↔ API  [impact H · likelihood H → P0]
- JWT bypass: alg=none; RS256→HS256 confusion (sign with public JWKS as HMAC key); wrong iss/aud;
  expired/exp; future nbf; unknown-kid → JWKS refresh flood (DoS); header-alias precedence
  (Authorization vs x-auth-token — which wins?).
- Tenancy: x-project-id = victim's project; UUID case/whitespace/leading-zero variants to dodge the
  owner check; enumeration via 404-vs-403 timing.
- Web: XSS via node name / ctx markdown / log line rendered in UI; Clerk token in URL/localStorage;
  CSRF via permissive CORS; clickjacking the OAuth device-code screen.

### TB2 API ↔ host ↔ engine (proxy)  [impact H · likelihood H → P0]
- Path traversal through authed proxy `/v1/projects/:id/engine/..%2f../` to reach another project's
  engine, `/v1/cli/*` (node-token surface), or `/ingress/*`; double-encoding; dot-segment normalization
  mismatch between API, host, and axum router.
- Public ingress `/p/:id/*` reaching engine `/v1/*` (control plane) instead of only `/ingress/*`.
- Header smuggling: client-sent `x-wheel-*` / `Authorization` / engine bearer surviving the proxy hop
  into the engine; A-user token + B-project id accepted on proxy/ingress/WS routes.
- WS: upgrade on a non-WS route; cross-tenant event stream; unauth WS.
- DoS: slowloris, body-size, capability `http` disabled still reachable.

### TB3 API ↔ secret custody  [impact Crit · likelihood M → P1]
- Engine/vault/host secrets leaking into logs, error bodies, `docker inspect` env (by-design residual,
  §"container"), or a sandbox's env; dev-mode default secret shipping to prod.

- **Defense-in-depth collapse (QA BUG-001, S2, SDK):** the exported JSON Schema accepts `..` in `endpoint.path`, vault VALUES in config, and unknown keys. The contract requires rejection by engine AND api; a permissive schema (Web's type source + the engine's validation gate) removes one of the two layers, so ingress/chest traversal loses its static half. Track: `qa/BUGS.md` 001.

### TB4 engine ↔ child process (the wire/token model)  [impact H · likelihood H → P0]
- Node-token: forge/guess (entropy?); reuse after node delete/rename/rewire; TOCTOU race between wire
  deletion and an in-flight CLI call.
- Privilege: `write` used where only `read` granted; `wheel query` SQL escape (ATTACH other db, PRAGMA,
  sqlite_master, read sibling `t_*` tables, CTE/UNION, load_extension, readfile()/writefile()).
- Namespace: chest path traversal (.., absolute, symlink, unicode-normalization, NUL); table `t_`-prefix
  collision / rename to shadow another node's table.

### TB5 child ↔ wheel CLI / MCP  [impact H · likelihood H → P0]
- Agent reads /data/wheel.db directly (bypassing wire checks) — file perms?
- Agent reaches control plane :7000 (docker) with its node token — is the node token accepted there, or
  only via /v1/cli/* with per-node scope?
- MCP `command` execs arbitrary binary as engine uid; script escapes timeout/output cap; fork bomb.
- **Envelope forgery (QA-rated S1):** body containing a literal `</AgentPrompt>` + a forged
  `<AgentPrompt from="user" type="user">` to impersonate the operator or a sibling node. Engine MUST
  escape close-tags and generate attributes; confirm no body text can forge attribution.
- **bypassPermissions inside sandbox:** if agents launch with `--permission-mode bypassPermissions`
  (or `--dangerously-skip-permissions`), a prompt-injected agent has unattended shell = full sandbox
  compromise with no human gate. The sandbox is the ONLY boundary; verify it is real (see TB6/TB7).

### TB6 container ↔ host (docker backend)  [impact Crit · likelihood M → P1]
- Docker socket reachable from host process = root-equiv; cap-drop / no-new-privileges / pids-limit
  actually applied (inspect, don't trust config); volume-name collision; ANSI/newline log injection
  into UI; env secrets in `docker inspect`.

### TB7 tenant ↔ tenant (process backend, prod)  [impact Crit · likelihood M → P0-once-exists]
- **Single kernel, single container, uid-per-project is the ONLY isolation.** Verify per-project uid
  enforced for EVERY child (agent, script, MCP). /data/projects/<other> mode-0700 + engine.sock owner
  actually block cross-uid; /proc/<pid>/environ of neighbours (needs hidepid=2); agents reaching
  *.railway.internal (Postgres A9, host :7100 A3) — network namespace? egress filter? rlimit/fork-bomb
  starving the whole machine; setuid-drop correctness (supplementary groups cleared, no_new_privs set).

### TB8 tool node → internet (§3d SSRF)  [impact H · likelihood H → P0]
- base_url / redirect target bypassing the public-IP check: DNS rebinding, IPv6 (`::ffff:127.0.0.1`,
  `[::1]`), octal/decimal/hex IP encodings, trailing-dot, `*.railway.internal`, `0.0.0.0`, redirect to
  169.254.169.254 (MOCKED); CRLF header injection via agent-filled param; agent overriding static/vault
  fill (dup keys, case variants, JSON-pointer collision); vault value echoed in response/log/curl;
  malicious spec DoS at import (YAML bomb, $ref loop, multi-MB body).

### TB9 capability delegation — `place` / `grant` / `manage` (new; board-as-code, operator/local runtime)  [impact H · likelihood H → P0]
New surface: an actor can `place` nodes and `grant` wires to other agents, `manage` (update/remove)
them, drive a board from a laptop (operator mode) or run `runtime:"local"` agents with their own login,
and export/import a whole board as `wheel.toml`. Delegation of capability is a classic escalation vector.
- **Attenuation break:** a granted wire must be *no stronger than the grantor's own* wire to that node.
  Attacks: agent with `read` on a vault/table grants itself or a confederate `write`/`send`; grant a
  wire the grantor doesn't hold at all; chain grants (A→B→C) to launder a capability past the attenuation
  check; type-confusion (grant `send` where only `read` is legal per the 9×9×3 matrix).
- **Authorization of grant/manage itself:** who may `grant`/`place`/`manage`? If any agent can, a
  prompt-injected agent rewrites the board (add an endpoint→script→vault exfil path, place a tool node
  with attacker base_url, wire itself to another agent's inbox). Contract says update/remove are
  **owner-only** — must be enforced server-side, not just UI-gated, and `place`/`grant` need the same.
- **board-as-code import (`wheel.toml`):** an imported template is attacker-controlled data — it must go
  through the SAME validation as API node/wire creation (wire matrix, name regex, SSRF on tool base_url,
  vault refs, no privilege the importer lacks). A template that declares wires the importer can't create
  must be rejected, not silently trusted. Malicious template DoS (huge/loopy toml) like the spec parser.
- **Operator mode & `runtime:"local"`:** commands from a laptop via API, and local agents with their own
  login, widen the auth surface — a local agent's token scope and a stolen/misused operator session must
  not exceed the same wire/owner checks; local runtime must not bypass the engine's server-side gating.
- **Budget stop:** a spend/stop control is a new DoS/abuse lever — can an agent evade its own budget
  stop, or trip another project's? Must be enforced by the engine/host, not the agent.
Owner: SDK/Engine (grant/place/manage enforcement, toml import validation) + API (operator-mode auth,
export/import routes, mail relay) — per PM's M3 scoping.

## 4. Priority ranking (impact × likelihood)
P0: TB1 (JWT/tenancy), TB2 (proxy/ingress), TB4 (wire/token), TB5 (envelope + bypassPermissions),
    TB7 (process-backend isolation, once it exists), TB8 (tool SSRF), TB9 (capability delegation: grant/place attenuation).
P1: TB3 (secret custody), TB6 (docker hardening).
P2: Web polish (clickjacking, CORS niceties), log-injection cosmetics.

## 5. Non-negotiable invariants to test continuously
1. Owner check (`project.owner_id == jwt.sub`) precedes every project op; non-owned → 404.
2. Node token authorizes ONLY the node's wire set; nothing accepts a raw engine/host secret from a child.
3. Engine delivery loop is the SOLE writer to a child's stdin (§3c#12).
4. `<AgentPrompt>` attributes are engine-generated and close-tags escaped — bodies cannot forge identity.
5. Vault values never appear in board state, logs, curl output, or tool responses.
6. Every SSRF-capable egress (tool base_url, redirects, mcp.url, script net) resolves to a public IP only.
7. In process backend, one tenant's uid can touch nothing of another tenant's (fs, socket, /proc, net).
8. A granted wire is never stronger than the grantor's own wire; place/grant/manage are
   owner-authorized server-side; imported `wheel.toml` passes the same validation as API creation.
