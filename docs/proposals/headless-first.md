# Headless-first `wheeld` and Docker

<!--
Copyright Morgan Metz
Licensed under the PolyForm Noncommercial License 1.0.0.
See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0
-->

Owner: API lane. Branch `api/headless-first`. Operator directive: *"wheeld and wheel through docker need to
be set up headless-first."* Orchestrator addition: API tokens are also how AgentGrid, a desktop client,
signs in to local `wheeld` and to the cloud API, so the token routes exist on every deployment.
ADVERSARY: the threat model below is the review target.

## What headless-first means

A person can install, operate and script Wheel with nothing but a shell. The web UI is an optional add-on
that talks to the API from its own server and is never needed to get a credential.

| # | Property | Observable definition |
|---|---|---|
| 1 | Loopback by default | `wheeld` with no `--bind`/`BIND_ADDR` listens on `127.0.0.1:8080`. Any bind whose host is not loopback logs one `WARN` line naming the address and what it exposes. |
| 2 | No browser needed | First boot on a store with no users creates a token-only owner account and writes an operator token to `<data-dir>/operator-token` (mode `0600`). The log names the path, never the value. `wheeld token create\|list\|revoke` manage tokens against the local store. Any authenticated client mints its own tokens over HTTP, on `wheeld` and on the cloud API alike. |
| 3 | No browser origins | wheeld's CORS allow-list is empty unless `CORS_ALLOWED_ORIGINS` says otherwise. Requests addressed to a host name nobody configured are refused (DNS rebinding). |
| 4 | Clean shutdown | SIGTERM/SIGINT stops every embedded engine, and every engine stops every agent's whole process group before `wheeld` exits. Stopping one project does the same for that project while the daemon keeps running. No orphans, no zombies. |
| 5 | Docker, headless | `docker/Dockerfile.wheeld` is one non-root image with `/data` as its volume and a healthcheck. Every documented `-p`/`ports:` is `127.0.0.1:`-only. The web UI is a compose profile. |

Email/password stays fully functional for the web UI. Signup stays open by default because the web UI and
QA's `WHEELD-*` smoke both depend on it. `WHEEL_SIGNUP=closed` closes it (see A3).

## 1. Loopback by default

- `crates/wheeld/src/config.rs`: the default bind is `127.0.0.1:8080`. `--bind` and `BIND_ADDR` still win.
- `0.0.0.0`, `[::]`, a LAN address, or any hostname other than `localhost`/`*.localhost` counts as exposed.
  At boot this prints one warning: the address, that anyone who can route to it reaches the sign-in,
  signup and public ingress routes, and that a container should publish the port on `127.0.0.1` only.
- The image sets `BIND_ADDR=0.0.0.0:8080`, because inside a container that is the container's network
  namespace. The warning fires there too, and its text covers that case.

## 2. API tokens

### Format and storage

- The token is `wht_` followed by 43 characters of base64url: 32 bytes from the OS RNG, 256 bits.
- The store keeps `sha256(token)` as lowercase hex. It never keeps the token.
- Why not argon2: the token has 256 bits of entropy, so a slow hash buys nothing, and a fast hash keeps
  verification to one indexed lookup.
- Why no pepper: keying the hash with the master key would add no strength at this entropy. It would also
  make the token store depend on a second secret.

```
api_tokens(id uuid PK, user_id text, name text, token_hash text UNIQUE, minted_by uuid NULL,
           created_at, last_used_at NULL, revoked_at NULL)
```

The schema is the same in `migrations/0004_api_tokens.sql` (Postgres) and
`migrations_sqlite/0004_api_tokens.sql`.

`user_id` is the verified subject, stored as text exactly like `projects.owner_id`: a local user's uuid
under `AUTH_MODE=local`, the identity provider's `sub` under `jwks`. So it has no foreign key to `users`.

### Verification (every `AUTH_MODE`)

`AuthUser` extraction works like this:

1. The token comes from `x-auth-token` or `Authorization: Bearer`. This is the existing
   `token_from_headers`, unchanged.
2. A `wht_` token takes the API-token path *before* the provider is consulted, in both modes.
   Anything else is verified as a session, exactly as before.
3. The API-token path runs one statement:
   `UPDATE api_tokens SET last_used_at = <db now> WHERE token_hash = $1 AND revoked_at IS NULL RETURNING user_id, id`.
   It checks revocation and records use atomically.
4. The lookup key is the token's SHA-256, never the token. How long an index probe takes can depend only
   on a digest the caller cannot aim at a stored one, so this hash lookup is the constant-time property.
   A second compare in Rust after an exact-match lookup could never fail a test, so it is not written.
5. No row and a revoked row both return the same `401`, with the same body as every other authentication
   failure. Nothing tells the caller that the token existed or was revoked.

### HTTP routes, every `AUTH_MODE`, scoped to the caller's own subject

| Route | Auth | Result |
|---|---|---|
| `POST /v1/auth/tokens {name}` | session or token | `201 {id, name, token, created_at}`. `token` is shown here and never again. Limited to 20 per account per hour; over that is `429`. |
| `GET /v1/auth/tokens` | session or token | `200 [{id, name, minted_by, created_at, last_used_at, revoked_at}]` |
| `DELETE /v1/auth/tokens/{id}` | session or token | `204`. Revokes the token **and every token it minted, transitively**. `404` if the token is not the caller's or does not exist. Idempotent. |

**Tokens may mint tokens, so revocation follows lineage.** AgentGrid holds a token and has to be able to
mint one per device without a browser. The cost is that a leaked token can mint successors. Recording
`minted_by`, and revoking a token's whole family with one recursive statement, gives revocation its point
back: revoking the leaked token revokes everything that was minted from it. The per-account rate limit
bounds how much a holder can fan out before anyone looks.

### First boot and the token-only owner

- When the API store has no users at all, `wheeld` creates `operator@wheeld.invalid` before it binds.
- The account's `password_hash` is a sentinel that is not a PHC string, so no password ever verifies.
  Refusing it burns the same argon2 work as a real check, so login timing does not say which accounts
  are token-only.
- `wheeld` then issues a token named `operator` and writes it to `<data-dir>/operator-token`. It removes any
  existing file and creates the new one with `O_EXCL` and mode `0600`, so an old file's looser mode can
  never be inherited.
- It logs `path=`, and never the value.
- If the process dies between creating the account and writing the file, the next boot sees users, writes
  nothing, and `wheeld token create` recovers.
- The owner is found by email **and** sentinel hash. Signup always stores an argon2 hash, so a remote
  signup that races for the owner address can never become the account that `wheeld token create` mints
  for.

### `wheeld token` (local store, data-dir trust boundary)

```
wheeld token create [--name <label>] [--email <account>]   # prints ONLY the token on stdout
wheeld token list                                          # id, account, name, created, last used, revoked
wheeld token revoke <id>                                   # and its whole lineage
```

These subcommands:

- open `STORE`, or `sqlite://<data-dir>/wheel.db` as `wheeld` itself composes it, and refuse to create a
  store that isn't there;
- work whether or not the daemon is running, because SQLite in WAL mode tolerates a second process;
- with no `--email`, target the token-only owner, and without an owner they refuse and name `--email`;
- need no HTTP and no running daemon. Whoever can read the data directory can already read `master.key`,
  which is strictly more powerful.

## 3. No browser origins

- wheeld has always read `CORS_ALLOWED_ORIGINS`, whose default is empty. The browser origins came from
  `infra/docker-compose.yml`'s defaults, which are now removed. A test pins that `composed_env` never
  sets the variable.
- **DNS rebinding.** An empty CORS list does not stop a rebinding page, because to the browser that
  request is same-origin. So wheeld wraps the API in a `Host` guard:
  - It admits any IP literal, `localhost` and `*.localhost`, the bind host when it is a name, and every
    name in `WHEEL_ALLOWED_HOSTS` (comma-separated).
  - An IP literal needs no allowance, because a browser sends one only when the page itself is at that
    address, which a rebinding page is not.
  - Any other name gets a `403` that names the variable, and so does a `Host` that is not text.
  - A request with no `Host` at all is not from a browser, and is admitted.
  - `/p/*` public ingress is exempt. It is public by design, and a rebinding page gains nothing there
    that any internet caller lacks.
- The web UI reaches the API only from its own server, as a server-to-server call.

## 4. Clean shutdown

The problems before this change:

- `EmbeddedSandbox::stop` called `JoinHandle::abort()`. That drops the engine's serve future, but the
  supervisor is kept alive by its own pump tasks, so the agent children kept running after a project was
  stopped, restarted or deleted.
- `wheel_engine::serve` returned without stopping agents.
- Agents shared the daemon's process group, so anything they spawned (tool shells, MCP servers) was
  outside any kill.

The design:

- **wheel-engine** gets `serve_until(cfg, shutdown)`, and `serve(cfg)` is `serve_until(cfg, SIGTERM|SIGINT)`.
  After the server drains, or fails, it calls the new `Supervisor::shutdown()`. This is the §4b spawn
  contract's "stop children" for the standalone engine binary too.
- Each agent is spawned as the leader of its own process group, via `process_group(0)` on the agent
  spawn only.
- `Supervisor::shutdown` sends `SIGTERM` to every live agent's process group at once and waits up to 3 s
  for the leaders. It then sends `SIGKILL` to each group and reaps the leader. It revokes each node token
  and marks each agent `parked`, so its session resumes on the next message after a restart.
- `stop`, `park`, `clear` and the api-key-only kill signal the group with `SIGKILL` instead of only the
  leader. It is the same bug class and the same one-line change at each site.
- **EmbeddedSandbox** keeps a oneshot sender per engine. `stop` fires it and awaits the task, falling back
  to `abort` after 15 s. `shutdown_all` stops every engine concurrently. `wheeld::run` calls it after the
  API has stopped accepting requests. Embedded engines install no signal handlers of their own any more.
- **Container PID 1** is `tini`. A grandchild that outlives its parent is reparented to PID 1 and reaped.

Proof, all with `qa/harness/fake-claude` as `claude`:

- A `wheeld` subprocess starts an agent, which spawns a background grandchild through `SH_B64`. After
  SIGTERM, both PIDs are gone.
- In process, `EmbeddedSandbox::stop` on a running project leaves no agent PID behind, and no zombie,
  while the test process lives on.

## 5. Docker and compose

- `docker/Dockerfile.wheeld` mirrors `docker/Dockerfile.host` instruction for instruction. It has the
  same build stage, the same runtime and toolchain layers (node, claude, codex, git, gh, rustup,
  build-essential, pnpm), and the same `agent` uid 10001. When both images are built on one machine,
  they share those layers from cache.
- It then adds `wheeld`, `wheel` and `tini`. It runs as `USER 10001`, declares `VOLUME /data`, sets
  `WHEEL_DATA_DIR=/data` and `BIND_ADDR=0.0.0.0:8080`, and checks health with
  `curl -fsS http://127.0.0.1:8080/healthz`.
- `infra/compose.wheeld.yml` is the headless quickstart:
  - the `wheeld` service, with its `wheel-data:/data` volume, published on `127.0.0.1:8080`, with
    `WHEEL_ALLOWED_HOSTS=wheeld` and a 30 s stop grace;
  - a `web` service under `profiles: ["web"]`, with `WHEEL_API_URL=http://wheeld:8080` and
    `WHEEL_AUTH_MODE=local`, published on `127.0.0.1:3000`.
- In `infra/docker-compose.yml` (the dev stack):
  - postgres is no longer published;
  - api is published on `127.0.0.1:8080`;
  - `CORS_ALLOWED_ORIGINS` defaults to empty;
  - the same `web` profile service is added, pointed at `http://api:8080`.
- `docker/Dockerfile.web` belongs to the Web lane (next section); this change only wires it in. The root
  `.dockerignore` now admits `web/`, minus `node_modules`, `.next` and test output.

## 6. Behind a reverse proxy (the VPS shape)

The operator's VPS runs on one box:

- `wheeld` bound to loopback;
- wheel-web on the same box, calling `wheeld` from its server;
- Caddy terminating TLS for one domain, routing `/v1/*` and `/p/*` to `wheeld` and everything else to
  wheel-web.

The kit itself (`infra/vps/*`) is the `api/vps-deploy` lane's. This lane owns the contract it consumes:

- **The trusted client address.** `X-Forwarded-For` is believed only when the TCP peer is inside
  `WHEEL_TRUSTED_PROXIES`, a comma-separated list of addresses or CIDRs. The default is to trust none.
  - The client is the first address, counting from the right, that is not a trusted proxy. So a
    client that prepends its own `X-Forwarded-For` is never reached.
  - A malformed hop ends the walk at the last address that could be vouched for.
  - `::ffff:`-mapped peers are canonicalised.
  - This is `wheel_api::http::client_ip`, applied by `wheeld` and by the `wheel-api` binary, both
    served with connect info.
- **Where the client address is used.** A public ingress hit carries it to the engine as
  `x-wheel-client-ip`, which keys the engine's per-caller ingress limit and `ip_allow`. The caller's own
  `x-wheel-*` headers are dropped before it is set, so only the API can set it.
  - Before this change nothing set that header, so every hit shared one "unattributed" bucket.
  - The API's own limits are keyed by email, account and project, so no forwarded header can buy
    capacity there.
- **`X-Forwarded-Proto` is never read.** The scheme the API advertises comes from `PUBLIC_BASE_URL`,
  which is configuration, not a claim a request can make.
- **`PUBLIC_BASE_URL`** is the base of every `ingress_base_url` and the issuer of local sessions.
  - `wheeld` defaults it to `http://localhost:<port>`: every earlier install used that, so sessions
    keep their issuer across the upgrade.
  - The kit sets it to `https://<domain>`. Changing it logs every session out, since the issuer moved;
    tokens are unaffected.
- **`WHEEL_ALLOWED_HOSTS=<domain>`.** Caddy passes the public `Host` through, and the DNS-rebinding
  guard refuses names it was not given.

## Contract the Web lane must match

- `docker/Dockerfile.web` is built from the **repository root** as context
  (`docker build -f docker/Dockerfile.web .`), like every other image here. It runs a standalone Next
  server on `0.0.0.0:3000` inside the container. It reads `WHEEL_API_URL` and `WHEEL_AUTH_MODE` at runtime,
  not at build time. Its healthcheck is `GET /version.json`.
- Every API call, including the events WebSocket and the ticket mint behind it, goes from the Next
  server. No `NEXT_PUBLIC_API_URL`, and no browser → API request. wheeld's default CORS list is empty,
  so a browser call will fail.
- The `Host` the server sends must be admitted. In compose it is `wheeld` via `WHEEL_ALLOWED_HOSTS`.
  Outside compose, `WHEEL_API_URL=http://127.0.0.1:8080` or `http://localhost:8080` works with no
  configuration.
- Credentials: an email/password session as today, or a `wht_` token. Both go as `x-auth-token` or
  `Authorization: Bearer`. A "create API token" UI shows the value once, from the `POST` response. There
  is no way to fetch it again.

## Threat model (ADVERSARY review target)

Assets:

- `master.key`: decrypts every project secret, and derives the session-signing key.
- The operator token and any `wht_` token, on `wheeld` and on the cloud API.
- Session JWTs.
- Project data under `<data-dir>/projects`.
- The operator's uid itself: every embedded agent runs as it.

| # | Adversary | Goal | Mitigation in this change | Residual |
|---|---|---|---|---|
| A1 | Network peer (LAN, café wifi, cloud VPC) | Reach the API or public ingress | Loopback default. A non-loopback bind is explicit and warned. Docs and compose publish only on `127.0.0.1` | A deliberate public bind is the operator's choice; TLS, firewalling and a reverse proxy are theirs to add |
| A2 | A web page in the operator's browser | CSRF, or scripting the API through DNS rebinding | Empty CORS list. JSON bodies force a preflight, which fails. The `Host` guard refuses the rebinding page's own name. Ingress `/p/*` is exempt, being public already | A page on an *admitted* host (`localhost:<other port>`, an IP literal) is a server on that address, which is inside the operator's trust domain |
| A3 | Another OS user on the same machine, without data-dir access | Get an account, then run agent code as the operator's uid | The data dir stays `0700` and the token file `0600`. `WHEEL_SIGNUP=closed` refuses `POST /v1/auth/signup` | **Open signup by default is local privilege escalation on a shared machine.** With the embedded backend, any account's agent runs as the daemon's uid. Default stays open, because the web UI and QA's smoke depend on it. Flipping the default is follow-up F2 and needs a ruling |
| A4 | Reader of a DB backup (`wheel.db` or the Postgres dump, without the token file) | Turn stored rows into credentials | Tokens are stored as SHA-256 only. Sessions need the master-key-derived key | A backup that includes the data dir includes `master.key` and `operator-token`: that reader is the owner |
| A5 | A holder of a leaked token (shell history, CI log, `ps`, a lost laptop running AgentGrid) | Keep or extend access | 256-bit secret with immediate revocation (checked on every request, no cache). **Lineage revocation**: revoking the leaked token revokes every token minted from it. Minting is limited to 20 per account per hour. `last_used_at` and `minted_by` make use and fan-out visible in `GET /v1/auth/tokens`. A token-only account has no password to change. The README quickstart keeps the token out of argv (`curl -H @<(…)`) | While live, a holder can mint (rate-limited) tokens and revoke the account's other tokens: they hold the account. A token placed in argv is visible via `ps` for the life of that process. Tokens do not expire (F4) |
| A6 | Timing observer on token verification | Recover a token or learn that one exists | The lookup key is `sha256(token)`, which an attacker cannot aim at a real token's hash. Every failure is one indistinguishable 401 | None known |
| A7 | An agent (untrusted code, §2) inside an embedded engine | Read secrets, act as the operator | Unchanged, and stated at boot: the embedded backend runs agents as the daemon's uid, so an agent can read `master.key` and `operator-token`. The token adds nothing an agent could not already forge from `master.key`. In Docker the blast radius is the container | Pre-existing: this is the embedded backend's documented trade. Per-node uids are §2/M3 |
| A8 | A container on the same compose network | Reach wheeld | Reachable only by service name, and only because `WHEEL_ALLOWED_HOSTS=wheeld`. Nothing else is in the headless compose project | The compose network is the operator's trust domain |
| A9 | Orphaned or zombie agent processes (cost and integrity, not an attacker) | Keep running, and keep spending, after stop | Process-group kill on stop, park and shutdown. Graceful drain on SIGTERM/SIGINT. `tini` as PID 1 in the image | `SIGKILL` of `wheeld` outside a container cannot clean up. Inside a container, PID-namespace teardown kills everything. `PR_SET_PDEATHSIG` was rejected: it fires when the spawning *thread* exits, and tokio's blocking threads come and go |
| A10 | Anyone reading logs | Harvest a token | Only the operator token's path is logged. `wheeld token create` prints the token on stdout alone, with diagnostics on stderr. No route returns a value after its `POST` | A terminal scrollback that captured `token create` holds the value |
| A11 | Remote signup racing for `operator@wheeld.invalid` | Become the account `wheeld token create` mints for | The owner is matched by email **and** by a sentinel hash that signup cannot produce. `.invalid` is reserved (RFC 2606) | None known |
| A12 | A user the identity provider has banned or signed out (cloud, `AUTH_MODE=jwks`) | Keep using the cloud API | Their sessions die at the provider, as before | **A `wht_` token does not consult the provider**, so it keeps working until revoked. Deprovisioning a user means revoking their tokens too (F6) |
| A13 | A public caller forging `X-Forwarded-For` or `x-wheel-client-ip` | Dodge the engine's per-caller ingress limit or `ip_allow`, or blame another address | `X-Forwarded-For` is read only from a peer inside `WHEEL_TRUSTED_PROXIES` (default none), right to left, stopping at the first untrusted hop. The caller's `x-wheel-*` headers are dropped and `x-wheel-client-ip` is set by the API alone. `X-Forwarded-Proto` is never read | An operator who trusts too wide a range (e.g. `0.0.0.0/0`) hands every caller the choice of address |

## Follow-ups (out of scope here)

- **F1 — `wheel` operator mode**: `wheel login` (store or accept a token), `wheel projects`, `wheel apply`
  (board/apply). Today `wheel` is agent-only, and it stays that way in this change.
- **F2 — close signup by default on `wheeld`** (A3). Needs a ruling. It needs `wheeld user add` so that
  web-UI users can still get a password account, and it needs QA's `WHEELD-signup` to move onto the
  operator token.
- **F3 — a web password for the token-only owner**, so that projects created headless show up in the web
  UI without a second account. Today: sign up in the web UI, then `wheeld token create --email <that
  address>`, and use that account from both sides.
- **F4 — token expiry and project-scoped tokens.**
- **F5 — one runtime base for `Dockerfile.host` and `Dockerfile.wheeld`** (SDK lane owns the host image).
  Until then they are kept instruction-identical so they share layers.
- **F6 — tokens follow the identity provider's user status** on the cloud API (A12): an admin
  "revoke every token of subject X", or a periodic check against the provider.
