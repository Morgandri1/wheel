# `api.wheel.dev` — public API

Stateless gateway in front of `wheel-host`. Owned by the API team. Contract: `ARCHITECTURE.md` §5.

The API never talks to a container runtime and never talks to an engine directly. Every sandbox
operation is an authenticated call to the single `wheel-host` machine over private networking.

## Authentication

Two providers, selected by `AUTH_MODE`. Both end at the same verified user id, and nothing
downstream — the ownership check above all — can tell which one ran. Swapping providers is
configuration, not code.

| `AUTH_MODE` | Token | Verified against |
|---|---|---|
| `local` (built in) | HS256, issued by this API | `SESSION_SECRET`, plus a live row in `sessions` |
| `jwks` | RS256, issued by an external provider | the provider's JWKS |

`AUTH_MODE` must be set explicitly in production — an unset value refuses to boot rather than
defaulting, because guessing wrong either rejects every real user or accepts tokens from the wrong
issuer. Under `jwks`, empty `CLERK_JWKS_URL`/`CLERK_ISSUER` also refuse to boot: a placeholder that
looks like configuration is worse than a missing one, since it starts and then rejects every token
for a reason nobody can see.

A token minted under one mode is rejected under the other. They are different algorithms verified
with different keys, so this needs no special case — it falls out of the design.

## Local auth routes (`AUTH_MODE=local`)

All five return `404` when `AUTH_MODE` is not `local`, so switching providers cannot leave a second
way in.

### `POST /v1/auth/signup`
```json
{ "email": "person@example.com", "password": "at least ten chars" }
```
`201` →
```json
{
  "token": "<HS256 session JWT>",
  "expires_at": "2026-09-12T18:40:00+00:00",
  "user": { "id": "<uuid>", "email": "person@example.com", "created_at": "..." }
}
```
Send `token` as `x-auth-token` on every subsequent request.

- Email is stored `citext`, so `Alice@x.com` and `alice@x.com` are one account. Without that,
  address casing silently creates a second user who cannot see the first one's projects.
- Password: 10–1024 **characters**, counted as characters rather than bytes so a ten-character
  passphrase in a non-Latin script is not wrongly rejected. No composition rules — requiring a digit
  and a symbol pushes people toward `Password1!`, which is measurably worse than length alone.
- Hashed with argon2id as a PHC string, so parameters can be raised later without invalidating
  existing rows.
- `409` if the email is taken. Rate limited globally per hour.

### `POST /v1/auth/login`
Same body. `200` with the same shape as signup.

**`401` for every failure** — unknown email, wrong password, malformed input — with one identical
body. Anything else confirms which addresses are registered.

Unknown emails are verified against a real argon2 hash before failing. Skipping the hash would make
login measurably faster for addresses that are not registered, turning the endpoint into an
account-existence oracle that timing alone would reveal.

Rate limited **per email**, 10 attempts per 15 minutes, counted in Postgres so the limit holds
across replicas rather than multiplying by the replica count. Per-email rather than per-IP because
the attack an IP limit misses is a password spray from many addresses against one account. Over the
limit returns `429`, not `401`: pretending the password was wrong would hide the lockout from a
legitimate user whose account someone else is attacking.

### `POST /v1/auth/logout`
`204`. Deletes the session row, so the token stops working immediately.

Succeeds even with an expired, already-revoked, or absent token — logging out should never fail.

This is why sessions are rows rather than pure stateless JWTs: a stateless token cannot be revoked
before it expires, and a "logout" that leaves the token working for seven more days is not a logout.

### `GET /v1/auth/me`
`200` → `{ "id", "email", "created_at" }`. `401` if the account no longer exists — the signature can
still verify over a user that has been deleted.

### `POST /v1/auth/password`
```json
{ "current_password": "...", "new_password": "..." }
```
`204`. Requires the current password even though the caller is already authenticated: that is what
stops a stolen session token from becoming a permanent takeover, since the attacker still cannot
lock the owner out.

**Revokes every session, including the caller's own.** If the password was changed because it was
compromised, leaving existing sessions alive would defeat the point. Clients must log in again.

Email-based password *reset* is M3 — it needs a mail provider, and there is none yet.

### Note on revocation vs. `session_version`

The review asked for a `users.session_version` counter. This implements the same guarantee with a
`sessions` table instead: logout and password change delete rows, and every request checks the row
is still live. That is strictly finer-grained — it can revoke one session without ending the others,
which a global version counter cannot — at the cost of one indexed lookup per request. Flagged
rather than silently substituted; say the word if you want the counter instead.


| Header | Required | Notes |
|---|---|---|
| `x-auth-token` | yes | Clerk session JWT (RS256). `Authorization: Bearer <jwt>` is accepted as an alias. |
| `x-project-id` | project-scoped routes | Must be a UUID. If the route also carries the id in its path, **the two must match exactly** or the request is rejected `400`. |
| `x-request-id` | no | Echoed back and attached to every log line for that request. |

Order of operations on every project-scoped request, without exception:

1. Verify the JWT signature against the cached Clerk JWKS (RS256 only).
2. Validate `iss`, `exp`, `nbf`, and `azp` when an allowlist is configured.
3. Load the project **with `owner_id = sub` as part of the query**.
4. Only then run the handler.

Step 3 is a `WHERE` predicate rather than a comparison after the fetch, so "no such project" and
"someone else's project" are literally the same code path. Both return `404`. There is no way to
learn whether an id exists.

### Failure modes

| Condition | Status | Body `error.code` |
|---|---|---|
| No token, malformed token, bad signature, expired, wrong issuer, unknown `kid` | `401` | `unauthorized` |
| Valid token, project not owned by `sub`, or no such project | `404` | `not_found` |
| `x-project-id` not a UUID, or disagrees with the path | `400` | `bad_request` |
| Ingress on a project with `capabilities.http = false` | `403` | `forbidden` |
| Per-user project cap reached | `409` | `conflict` |
| Body over the cap (5 MiB default) | `413` | `payload_too_large` |
| Ingress over the rate limit | `429` | `rate_limited` |
| Host unreachable | `502` | `bad_gateway` |
| Host too slow (30 s default) | `504` | `gateway_timeout` |

The 401 body is identical for every cause. The specific reason is logged for operators but never
returned, so the response cannot be used as an oracle for which part of a forged token was wrong.

### Error body

Every error, on every route:

```json
{ "error": { "code": "not_found", "message": "The requested resource does not exist." } }
```

## Routes

### `GET /healthz`
Unauthenticated. `200 {"status":"ok"}`.

### `POST /v1/projects`
```bash
curl -X POST https://api.wheel.dev/v1/projects \
  -H "x-auth-token: $JWT" -H 'content-type: application/json' \
  -d '{"name":"my board"}'
```
`201` → `Project`. Name is 1–64 characters, no control characters. Generates the project's engine
secret and vault key, stores them encrypted (AES-256-GCM under `API_MASTER_KEY`), registers the
sandbox with the host, and returns before the sandbox is necessarily running — poll `GET` for
`status`.

### `GET /v1/projects`
`200` → `[Project]`, newest first. Only the caller's own projects. `x-project-id` not required.

### `GET /v1/projects/{id}`
`200` → `Project`, with `status` reconciled against what the host actually reports.

### `PATCH /v1/projects/{id}`
```json
{ "name": "renamed", "capabilities": { "http": true } }
```
Both fields optional. Setting `capabilities.http = true` is what opens the public ingress route.

### `DELETE /v1/projects/{id}`
`204`. Stops and destroys the sandbox and its data, then deletes the row. The sandbox is torn down
first: if that fails the row is kept, because a sandbox we no longer have a record of is one nobody
will ever clean up.

### `POST /v1/projects/{id}/start` · `/stop` · `/restart`
`200` → `Project`. `start` blocks until the engine reports healthy (up to ~30 s) or the host
returns a timeout.

### `ANY /v1/projects/{id}/engine/{*rest}`
Authenticated proxy to the project's engine control plane (`ARCHITECTURE.md` §4). Ownership is
proven before any byte is forwarded.

```bash
curl https://api.wheel.dev/v1/projects/$PID/engine/v1/board -H "x-auth-token: $JWT"
```

WebSocket upgrade is supported for `/engine/v1/events` and bridged in both directions. Frames are
relayed verbatim, without inspection or re-encoding — the `message` event in particular must reach
the UI unmodified so a row can be correlated by its id.

Header hygiene, both directions:
- Hop-by-hop headers are dropped, **including any header the client names in `Connection`**.
- The client's `x-auth-token` / `Authorization` never reach the host. The host authenticates the
  API, not the end user; relaying a user credential downstream is how replay bugs begin.
- `WHEEL_HOST_SECRET` is attached upstream and never appears in a response.

### `ANY /p/{project_id}/{*rest}` — public ingress
**Unauthenticated by design.** Reaches the project's `endpoint` nodes.

- `404` if the project does not exist.
- `403` if `capabilities.http` is false (the default, and also the result of a malformed
  capabilities blob — this fails closed).
- Rate limited per project, default 60 req/min, `429` when exceeded.
- Body capped at 5 MiB, `413` when exceeded.
- Every `x-wheel-*` header from the caller is stripped before we add `x-wheel-ingress: 1`, so a
  public caller cannot forge the marker the engine trusts.

Counting happens only after the project is known to exist, so traffic aimed at random UUIDs cannot
make us write unbounded counter rows.

## Rate limiting across replicas

The limiter is a fixed-window counter in Postgres, not an in-process bucket. With N replicas behind
a load balancer an in-memory limit silently becomes N × the configured value — the control weakens
exactly as you scale, which is backwards. The window boundary is computed with the *database's*
clock so replicas agree.

Known tradeoff: a fixed window admits the classic boundary burst, up to 2× the limit across two
adjacent windows. Accepted for v1 — this exists to stop sustained abuse of an unauthenticated
route, not to smooth traffic. A sliding window in Redis is the upgrade path.

## Configuration

| Variable | Required | Default | Notes |
|---|---|---|---|
| `WHEEL_ENV` | no | `prod` | `dev` or `prod`. Anything else refuses to boot. Unset means prod. |
| `STORE` | yes* | — | `postgres://…` or `sqlite://path/to/wheel.db`. The scheme picks the backend, so there is no mode flag that can disagree with the connection string. |
| `DATABASE_URL` | yes* | — | Accepted as an alias for `STORE`; production, the compose stack and every deploy already set it. (*one of the two is required.) |
| `CLERK_JWKS_URL`, `CLERK_ISSUER` | yes | — | |
| `CLERK_AZP` | no | — | Comma-separated `azp` allowlist. |
| `API_MASTER_KEY` | yes | — | 32 bytes, base64. `openssl rand -base64 32`. |
| `WHEEL_HOST_URL` | yes | — | e.g. `http://wheel-host.railway.internal:7100`. |
| `WHEEL_HOST_SECRET` | yes | — | Bearer for the host. Must never appear in a sandbox's environment. |
| `AUTH_DEV_SECRET` | no | — | HS256 test tokens. **Only honoured when `WHEEL_ENV=dev`.** |
| `CORS_ALLOWED_ORIGINS` | no | — | Comma-separated exact origins. |
| `MAX_PROJECTS_PER_USER` | no | `20` | |
| `INGRESS_RATE_PER_MIN` | no | `60` | `0` disables. |
| `INGRESS_BODY_LIMIT_BYTES` | no | `5242880` | |
| `PROXY_TIMEOUT_SECS` | no | `30` | Not applied to WebSockets or log streams. |
| `HOST_CONNECT_TIMEOUT_SECS` | no | `3` | How long to wait for a TCP connection to the host before calling it unreachable. Separate from `PROXY_TIMEOUT_SECS` on purpose — see below. |

### The dev-bypass interlock

`AUTH_DEV_SECRET` accepts HS256 tokens, which anyone holding the secret can mint for any `sub`. It
is a complete authentication bypass, deliberately, for local testing.

**If it is set while `WHEEL_ENV` is not `dev`, the process refuses to start.** An unset `WHEEL_ENV`
counts as prod, because in practice nobody sets `WHEEL_ENV=prod` by hand — they just don't set it,
and that must not be the permissive case. Covered by `tests/config_interlock.rs`.

### Why connecting and responding have different timeouts

A slow *response* is normal: a project start legitimately blocks while an engine boots. A slow
*connect* means the host is not there, and waiting does not help. They were once the same 30s, and
when the host went down, `POST /v1/projects` sat for the full request timeout until the platform
edge returned its own 502 — so the browser got an edge error page instead of our error envelope and
the UI simply hung. Connect now gives up after `HOST_CONNECT_TIMEOUT_SECS`, and the outage is
reported as a project in `error` state with a body the client can read.

### Two backends, one API

Postgres is production. SQLite exists so a local or open-source install has no dependency to stand
up first — it is what `wheeld` uses by default, and it is a real backend rather than a stub: the
same routes, the same migrations in `migrations_sqlite/`, and the DB-backed test suites run against
both so parity is proven rather than assumed.

Where the SQL differs it is written out per dialect, with `Db::pick` choosing between two named
statements rather than a string being assembled. The differences are deliberate rather than
incidental:

* **The rate limiters.** The window boundary comes from the *database* clock on both, because the
  API runs as N replicas whose own clocks may differ by seconds and a boundary they disagree about
  is a limit that admits more than it says. Postgres truncates with `date_trunc`; SQLite has no
  such function and uses `strftime` to the same effect.
* **Timestamps stay on the database side.** SQLite has no `now()`, and its `CURRENT_TIMESTAMP` is a
  different text format from the one the driver writes — comparing against that would be a
  lexicographic accident rather than a comparison. The SQLite statements use
  `strftime('%Y-%m-%dT%H:%M:%fZ', 'now')`, which produces exactly the format stored, so both
  backends keep reading the clock the rows were written against.

A duplicate-email signup is a 409 on both, which needs saying because the two report the constraint
differently (`23505` vs `2067`/`1555`); without that mapping a local install would answer 500 where
production answers 409, and "works locally" would stop meaning anything.

### The production identity-provider interlock

Under `AUTH_MODE=jwks` with `WHEEL_ENV=prod`, `CLERK_JWKS_URL` and `CLERK_ISSUER` must both be
`https://` and must not point at loopback, RFC1918, link-local, unique-local IPv6, or a `.local` /
`.internal` name. Otherwise the process refuses to start.

This is not tidiness. A stub identity provider does not fail closed — it authenticates everyone, as
whoever the caller claims to be, and the ownership checks then work perfectly against an identity
the attacker chose (ADVERSARY 017, where a mock-auth build resolved every token to a single
`owner_id`). Boot is the only place to catch it. Dev is unaffected: pointing at a local issuer is
exactly what dev is for. The host is checked as a literal, without DNS — boot is not the place to
trust a resolver, and a name that resolves publicly today may not tomorrow.

## CORS

Explicit origin allowlist from `CORS_ALLOWED_ORIGINS`. Never wildcard-with-credentials: the web app
authenticates with a header rather than cookies, so `allow_credentials` is never needed, and an
explicit list keeps a hostile page from scripting the API with a user's token.

## Running the API natively (no Docker) — for the web team

The containerised stack rebuilds the Rust image on every change, which is a poor inner loop for
frontend work. This runs the same two binaries directly against the stub engine.

```bash
cargo build -p wheel-api -p wheel-host

# 1. Postgres (any local instance; create a database first)
#    docker run -d --name wheel-pg -p 55432:5432 \
#      -e POSTGRES_USER=wheel -e POSTGRES_PASSWORD=wheel -e POSTGRES_DB=wheel_dev postgres:17-alpine

# 2. Stub engine on :7000
python3 infra/dev/stub_engine.py &

# 3. Host on :7100
WHEEL_ENV=dev SANDBOX_BACKEND=external ENGINE_BASE_URL=http://127.0.0.1:7000 \
WHEEL_HOST_SECRET=dev-host-secret-at-least-16-chars BIND_ADDR=127.0.0.1:7100 \
WHEEL_DATA_DIR=/tmp/wheel-host-data ./target/debug/wheel-host &

# 4. API on :8080
WHEEL_ENV=dev BIND_ADDR=127.0.0.1:8080 \
DATABASE_URL=postgres://wheel:wheel@127.0.0.1:55432/wheel_dev \
CLERK_ISSUER=https://dev.wheel.local AUTH_DEV_SECRET=dev-only-hs256-secret \
API_MASTER_KEY=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA= \
WHEEL_HOST_URL=http://127.0.0.1:7100 WHEEL_HOST_SECRET=dev-host-secret-at-least-16-chars \
CORS_ALLOWED_ORIGINS=http://localhost:3000 ./target/debug/wheel-api
```

`SANDBOX_BACKEND=external` points the host at an engine someone else started, instead of creating
containers. It refuses to load unless `WHEEL_ENV=dev`, because it performs no isolation at all.

### Minting a token without Clerk

With `AUTH_DEV_SECRET` set and `WHEEL_ENV=dev`, the API accepts HS256 tokens. `sub` is the user id,
so two different `sub` values are two different tenants — which is how to exercise the ownership
boundary locally. Claims: `{sub, iss, exp, nbf}`, where `iss` must equal `CLERK_ISSUER` exactly.
A ten-line reference implementation lives in `infra/dev/e2e.py` (`mint()`); copy it rather than
rewriting it.

Sanity check the whole chain, including the boundary cases, with:

```bash
python3 infra/dev/e2e.py
```

### Opening the events WebSocket

Browsers cannot set headers on a WebSocket handshake, and the session JWT must never appear in a
URL where it would be captured by proxy and server logs. So the socket is opened with a ticket:

```
POST /v1/projects/{id}/ws-ticket        -> { "ticket": "...", "expires_in": 30 }
ws://localhost:8080/v1/projects/{id}/engine/v1/events?ticket=<ticket>
```

The ticket is single-use, expires in 30 seconds, and is bound to the (user, project) pair it was
minted for.

## Local development

```bash
export API_MASTER_KEY=$(openssl rand -base64 32)
export CLERK_JWKS_URL=... CLERK_ISSUER=...
docker compose -f infra/docker-compose.yml up --build
```

The docker socket is mounted into the **host** service only. Anything that can reach the socket can
trivially escape to the machine, so the internet-facing API must never see it.

With `WHEEL_ENV=dev` and `AUTH_DEV_SECRET` set, HS256 tokens are accepted and the JWKS endpoint is
never contacted. `iss` is still validated, so a minted token must carry exactly `CLERK_ISSUER`.

### End-to-end check

`infra/dev/e2e.py` mints a dev token and walks the whole chain: create project → start sandbox →
read the board back through the authenticated proxy. It also asserts the boundary holds — no token
is `401`, another user's project is `404` (never `403`, which would confirm existence), and ingress
on a project that has not opted in is `403`.

```bash
python3 infra/dev/e2e.py
```

Until SDK's engine lands, `infra/dev/Dockerfile.engine.stub` provides a stub that implements just
enough to prove the chain: an unauthenticated `/healthz` for the host's readiness probe and a
bearer-gated `/v1/board`. `infra/dev/Dockerfile.host.dev` likewise builds only the supervisor and
is replaced by SDK's `docker/Dockerfile.host` when that exists.
