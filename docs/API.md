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
| either | `wht_…` API token, issued by this API | its SHA-256 in `api_tokens`, not revoked (see [API tokens](#api-tokens-every-auth_mode)) |

`AUTH_MODE` must be set explicitly in production — an unset value refuses to boot rather than
defaulting, because guessing wrong either rejects every real user or accepts tokens from the wrong
issuer. Under `jwks`, empty `CLERK_JWKS_URL`/`CLERK_ISSUER` also refuse to boot: a placeholder that
looks like configuration is worse than a missing one, since it starts and then rejects every token
for a reason nobody can see.

A token minted under one mode is rejected under the other. They are different algorithms verified
with different keys, so this needs no special case — it falls out of the design.

## Local auth routes (`AUTH_MODE=local`)

All six return `404` when `AUTH_MODE` is not `local`, so switching providers cannot leave a second
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
- `403`, with the generic `forbidden` body, when signup is closed. See the signup policy below.

### Signup policy (`WHEEL_SIGNUP`)
`closed` (the default: unset or empty) or `open`, for the `wheel-api` binary and `wheeld` alike. Any
other value refuses to boot, and `invite` is refused with a pointer to the route below.

- **Closed:** `POST /v1/auth/signup` answers `403` before it counts against the signup limit, and
  creates nothing.
- **Closed is not inferred from how the box looks.** An earlier version opened it on a loopback-only
  `wheeld`, and a review showed that is still reachable by every account on the machine and by any
  proxy or tunnel that presents `127.0.0.1`. Opening it is a decision made out loud:
  `WHEEL_SIGNUP=open`.
- **Why:** the embedded backend runs every account's agents as the daemon's own user. An open signup
  on a public box is a stranger's code on it.

### `POST /v1/auth/users`
The owner adds an email/password account, whatever the signup policy. Same body as signup. `201` →
`{ "id", "email", "created_at" }`, with no session: the new user signs in themselves.

- **Only the owner may call it:** the token-only account `wheeld` created on first boot, the one
  account a signup can never produce. Anyone else gets `403`, including people the owner added and
  their tokens, so a closed signup cannot be reopened from the inside.
- `401` without a credential; `409` if the email is taken; `400` for a bad email or password.
- `404` when `AUTH_MODE` is not `local`.

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
`200` → `{ "id", "email", "created_at", "owner" }`. `owner` is true for the token-only owner, the
one account that may call `POST /v1/auth/users`. `401` if the account no longer exists — the
signature can still verify over a user that has been deleted.

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

## API tokens (every `AUTH_MODE`)

API tokens are how a client without a browser signs in: a script, CI, or a desktop app such as AgentGrid. They work the
same way against a local `wheeld` and the cloud API. Design and threat model: `docs/proposals/headless-first.md`.

- **Format.** `wht_` followed by 43 base64url characters, which encode 32 random bytes.
- **Sending one.** Use it wherever a session goes: `x-auth-token: wht_…` or `Authorization: Bearer wht_…`.
- **Subject.** A token speaks for the account that minted it. Under `local` that is the user's id; under `jwks` it is the
  identity provider's `sub`. Projects and ownership checks cannot tell a token from a session.
- **Storage.** The server keeps only the token's SHA-256, which is also the lookup key. The value is returned once, when
  it is minted, and never again: not by any route, and not in any log.
- **Failure.** An unknown or revoked token gets the same `401`, with the same body, as every other authentication failure.
  Nothing tells a caller that the token existed or was revoked.
- **Last use.** Every authenticated request stamps `last_used_at`. The revocation check and the stamp are one statement.
- **A dead family stays dead.** A token authenticates only while every token in its minting chain is
  unrevoked. That covers a child minted in the instant before its parent's revocation landed, which the
  revocation itself could not see.
- **A password change ends what that account's sessions minted,** and everything those tokens minted,
  along with the sessions themselves. Tokens the operator minted from the data directory
  (`wheeld token create`, the first-boot operator token) are not a session's, and survive it.

### `POST /v1/auth/tokens`
Authenticated by a session or by an existing token.
```json
{ "name": "laptop" }
```
`201` →
```json
{ "id": "<uuid>", "name": "laptop", "token": "wht_…", "created_at": "2026-09-11T10:00:00+00:00" }
```
- `name` is 1–64 characters after trimming, with no control characters. Otherwise `400`.
- Minting is rate limited to **20 per account per hour**, counted in the database like the login limit. Over the limit
  returns `429`.
- A token minted *by a token* records its parent as `minted_by`.

### `GET /v1/auth/tokens`
`200` → the caller's own tokens, newest first. It never includes a token value or a hash.
```json
[ { "id": "<uuid>", "name": "laptop", "minted_by": null,
    "created_at": "…", "last_used_at": "…" | null, "revoked_at": "…" | null } ]
```

### `DELETE /v1/auth/tokens/{id}`
`204`. The token stops working on the next request.

- **It revokes the token's whole lineage**: every token it minted, transitively. Whoever used a leaked token to mint
  successors loses them with it, so revoking the leak ends it.
- It is idempotent, and a second revoke keeps the first `revoked_at`.
- It returns `404` for a token that belongs to someone else, exactly as for one that does not exist or an id that is not
  a uuid.

### `wheeld`: the operator token and `wheeld token`
On its first start against a store with no accounts, `wheeld` does three things:
1. It creates `operator@wheeld.invalid`, an account with no password that signs in only with tokens.
2. It writes that account's first token to `<data-dir>/operator-token` (`0600`).
3. It logs the path, never the value.

`wheeld token create [--name N] [--email E] | list | revoke <id>` manages tokens against the local store directly. It
needs no running daemon and no HTTP. Access to the data directory is the boundary, the same one that guards `master.key`.
With no `--email`, `create` mints for the token-only owner.

### Note on revocation vs. `session_version`

The review asked for a `users.session_version` counter. This implements the same guarantee with a
`sessions` table instead: logout and password change delete rows, and every request checks the row
is still live. That is strictly finer-grained — it can revoke one session without ending the others,
which a global version counter cannot — at the cost of one indexed lookup per request. Flagged
rather than silently substituted; say the word if you want the counter instead.


| Header | Required | Notes |
|---|---|---|
| `x-auth-token` | yes | A session JWT (local HS256 or the provider's RS256), or a `wht_` API token. `Authorization: Bearer <token>` is accepted as an alias. |
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
Unauthenticated. `200 {"status":"ok","auth_mode":"local"|"jwks"}`.

`auth_mode` is the mode this API is actually running, and it is published so a client can assert it
agrees. If the web build ships `clerk` while the API runs `local`, the user gets a login widget whose
token we reject, or a form talking to a verifier that is not running — two correct halves, a
deploy-time disagreement, and nothing either side tests alone can see. The web smoke path compares
this against `NEXT_PUBLIC_AUTH_MODE` and goes red on a mismatch.

**The mode and nothing further.** Not the issuer, not the JWKS URL, not key material. Publishing the
mode reveals nothing that `POST /v1/auth/login` answering `401` rather than `404` does not already
reveal, and that argument covers the mode exactly. `tests/healthz_auth_mode.rs` holds the response to
those two keys, so a future field cannot be added to an unauthenticated probe by accident.

### `GET /v1/host/healthz`

Liveness of the sandbox host, as seen from the API. Unauthenticated, like `/healthz`.

```
200 {"ok": true}      the host is serving
503 {"ok": false}     it is not
```

It exists because `GET /healthz` answering 200 does not mean the product works: during one outage
the API stayed perfectly healthy while the host was stopped, and every project create hung until
the platform edge gave up. The host has no public domain, so nothing outside the API can ask it
directly.

Liveness only — no backend name, no project counts, no upstream error text. The answer is cached for
one second: the route is unauthenticated, and without that a flood here becomes a flood against the
one machine every tenant's sandbox runs on.

### `POST /v1/projects`
```bash
curl -X POST https://api.wheel.dev/v1/projects \
  -H "x-auth-token: $JWT" -H 'content-type: application/json' \
  -d '{"name":"my board"}'
```
`201` → `Project`. Name is 1–64 characters, no control characters. Generates the project's engine
secret and vault key, stores them encrypted (AES-256-GCM under `API_MASTER_KEY`), registers the
sandbox with the host, **and starts it** — a new project comes back `running` and its engine answers
immediately (`ARCHITECTURE.md` §6 M1: "create project → sandbox starts").

The create still succeeds if the sandbox does not come up: the row exists, so `201` is the honest
answer, and the returned `status` is `error` rather than `stopped`. Retry with `POST
/v1/projects/{id}/start`. A `status` of `starting` means the host accepted the start but the engine
had not reported healthy when we looked — poll `GET`.

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

Two failures are worth knowing by name, because both were once reported as something else:

- **The host is still restoring.** For a short window after the host restarts, project routes answer
  `503` and the body says how far through it is (`restored` of `to_restore`). A host working through
  its list and a host wedged on the first project used to be the same response; retry rather than
  treat it as an error.
- **The volume is full.** The host refuses a start below its free-space floor rather than launching
  a sandbox that will corrupt its own database on the first write. `POST /start` returns `507
  insufficient_storage`; a `POST /v1/projects` that hits it returns `201` with `status: "error"`,
  because the project was created either way. This is the failure that took production down once
  already, wearing a sqlite error about shared memory as a disguise, and it is reported by name so
  the next one is not a 500 with no cause.

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

### Confirming which code a project's engine is running

`GET /v1/projects/{id}/engine/v1/healthz` (through the proxy above) returns the engine's own health,
including a `build` field:

```json
{"build":"4af95c8f0ba604c72481e98e7abc0d7725a2e0d4","ok":true,"stalled":[],"version":"0.1.0"}
```

**It is an owner-readable fact, not a public one.** The API's own `/healthz` and `/v1/host/healthz`
do not carry it — the latter deliberately, since it is liveness-only for an unauthenticated caller.
Reading `build` requires the project owner's token.

**What the number means depends on how the image was built**, and the difference matters at exactly
the moment you are relying on it:

| image built by | `build` reports |
|---|---|
| `make engine-image`, or any build passing `--build-arg GIT_SHA` (this is what CI does) | the exact commit the binaries were **compiled from** |
| Railway | `"unknown"` — it builds `docker/Dockerfile.host` directly and passes no build args, so `ARG GIT_SHA` keeps its default |

The value is baked at **compile** time (`option_env!` reading the build stage's `ENV`), so nothing at
runtime can change it. Verified by running the real image: with `--build-arg` the field matched
`git rev-parse HEAD` exactly, while the container's own `WHEEL_BUILD_SHA` was unset — the field is
right *because* it was compiled in, not because the environment supplied it.

So on today's production deploys `build` reads `"unknown"`. That is honest rather than misleading —
it declines to name a commit rather than naming the wrong one — but it means **`build` cannot yet
confirm a Railway deploy**. Confirm those by checking that the deploy actually rebuilt.

Making it exact in production requires passing `GIT_SHA` as a Railway build arg. Whether Railway
supports that has now been **verified: it does not**, by two mechanisms — see
`infra/railway/README.md`. So `build` reading `"unknown"` in production is the honest end state
rather than a gap awaiting a fix.

### `ANY /p/{project_id}/{*rest}` — public ingress
**Unauthenticated by design.** Reaches the project's `endpoint` nodes.

On success the engine's own response is returned unchanged — status, headers and bytes. For a hit on
an `endpoint` node whose wires deliver to agents, that is:

```json
202 {"accepted": true, "queued": 1}
```

`queued` is the number of messages **enqueued**, which is what has happened at the moment the
response is written. Nothing has been delivered yet: the pump is asynchronous, and the target agent
may be parked, unauthenticated or mid-turn. The field is deliberately not called `delivered` — it was
once, and production answered `delivered: 1` for a message that never reached a child, which a
webhook provider reads as success and never retries.

The `202` is not a promise that an agent acted on the hit, only that the hit is durably queued. To
observe real delivery, watch the `message` event on the events WebSocket, where the state machine is
`queued → delivered → consumed` (§3c#4). An endpoint with `response_mode: script` returns the
script's own output instead of this envelope.

Failure cases:

- `404` if the project does not exist.
- `403` if `capabilities.http` is false (the default, and also the result of a malformed
  capabilities blob — this fails closed). The toggle is `capabilities.http` on `PATCH
  /v1/projects/{id}`.
- `501 ingress_unavailable` if the engine answers with a **bodiless** 404 — it has no `/ingress/*`
  route at all, so the path is not the problem and saying "not found" invites the reader to hunt for
  a typo. A 404 the engine actually wrote (its `no_such_endpoint`, or an endpoint script's own)
  passes through unchanged.
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
| `CORS_ALLOWED_ORIGINS` | no | empty | Comma-separated exact origins. Empty means no browser may call the API directly: the web UI calls it from its own server. |
| `MAX_PROJECTS_PER_USER` | no | `20` | |
| `INGRESS_RATE_PER_MIN` | no | `60` | `0` disables. |
| `INGRESS_BODY_LIMIT_BYTES` | no | `5242880` | |
| `PROXY_TIMEOUT_SECS` | no | `30` | Not applied to WebSockets or log streams. |
| `PUBLIC_BASE_URL` | no | `http://localhost:8080` | The public base of every `ingress_base_url` (`<PUBLIC_BASE_URL>/p/<id>`, returned by every project route, including create), and the issuer of local sessions. Clients display it as-is: the web app no longer builds it. Behind TLS, `https://<domain>`. Changing it ends every session: the issuer moved. `wheeld` defaults it to `http://localhost:<port>` of its bind. |
| `WHEEL_SIGNUP` | no | `closed` | `closed` or `open`, local auth only. Unset or empty is closed. See [Signup policy](#signup-policy-wheel_signup). |
| `WHEEL_TRUSTED_PROXIES` | no | none | Comma-separated addresses or CIDRs of reverse proxies whose `X-Forwarded-For` is believed. See [Behind a reverse proxy](#behind-a-reverse-proxy). A malformed entry refuses to boot. |
| `HOST_CONNECT_TIMEOUT_SECS` | no | `3` | How long to wait for a TCP connection to the host before calling it unreachable. Separate from `PROXY_TIMEOUT_SECS` on purpose — see below. |

### Running `AUTH_MODE=jwks` without a provider account

`cargo run -p wheel-api --example stub-issuer` serves a JWKS on `127.0.0.1:9911` and prints a ready
token, so `jwks` mode can be exercised with no Clerk account:

```
AUTH_MODE=jwks
CLERK_JWKS_URL=http://127.0.0.1:9911/jwks
CLERK_ISSUER=https://clerk.example.test
```

`GET /token?sub=<id>` mints more. `PORT` and `SUB` override the defaults.

It signs with the fixture key in `crates/wheel-api/tests/fixtures/`, so a token it mints and a token
the test suite mints are signed by the same key. On startup it puts a token through
`auth::claims::verify` — the real verifier, not a copy — and refuses to serve if that fails, so a
drift between the JWKS document and what the API accepts is caught here rather than somewhere less
obvious.

**Never in production.** It is an `examples/` target, so it is in no shipped binary and unreachable
from the library, and its signing key is committed to this repository in plain text — anyone can mint
any `sub`.

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
same routes and the same schema, translated in `migrations_sqlite/`.

How much parity is actually proven, stated precisely because "both backends are tested" is the kind
of claim that gets believed:

* `sqlite_parity.rs` drives the **real router** against SQLite — signup, login, `/auth/me`, logout,
  password change, project create/list/rename, the per-user cap, the cross-tenant 404, the shared
  login limiter and the maintenance sweeps. It needs no `TEST_DATABASE_URL`, so it runs everywhere.
* `ratelimit_db.rs` runs its assertions against both backends in one pass.
* `sqlite_store.rs` and `sqlite_dialect.rs` pin the schema and the dialect assumptions the shared
  SQL rests on, against a real database.
* The remaining `*_db.rs` suites are **Postgres-only** and skip without `TEST_DATABASE_URL`. Their
  SQLite coverage is whatever `sqlite_parity.rs` reaches, which is the routes above and not more.

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

## Behind a reverse proxy

Behind a proxy, the TCP peer is the proxy, and the caller's address is whatever `X-Forwarded-For`
says. A client can write that header itself. So:

- **`X-Forwarded-For` is believed only from a peer inside `WHEEL_TRUSTED_PROXIES`.** The default
  trusts no one, and then the peer is the client.
- **The client is the first address, counting from the right, that is not a trusted proxy.** A value
  a client prepended is never reached. A hop that is not an address ends the walk at the last one that
  could be vouched for.
- **A public ingress hit carries that address to the engine as `x-wheel-client-ip`.** The caller's
  own `x-wheel-*` headers are stripped first, so only the API can set it. The engine's per-caller
  ingress limit and an endpoint's `ip_allow` key on it.
- **`X-Forwarded-Proto` is never read.** The scheme the API advertises comes from `PUBLIC_BASE_URL`.

Both the `wheel-api` binary and `wheeld` apply this, and both are served with the peer address
available to it. For a proxy on the same machine, `WHEEL_TRUSTED_PROXIES=127.0.0.1/32,::1`.

## Cookies are never credentials

The API reads a credential from `x-auth-token` or `Authorization: Bearer` and from nothing else.
There is no cookie path, now or ever. This is pinned by `tests/no_cookie_auth.rs`: a valid session
JWT or `wht_` token placed in a cookie gets `401` on every route, and a cookie-only logout revokes
nothing.

This matters behind the VPS proxy. `/v1` shares an origin with the web app there, so browsers attach
the web's `__Host-wheel_session` cookie to every `/v1` request, including ones a hostile page
causes. A cookie that authenticated would be ambient authority over the whole API. The VPS kit also
strips `Cookie` on `/v1` and `/p` at the proxy, as a second line; this rule is the first.

## CORS

The web UI calls the API from its own server, so its origin needs no CORS grant. The default
allow-list is empty, and the VPS deployment sets none. `CORS_ALLOWED_ORIGINS` exists for a browser
client someone is deliberately developing against the API directly.

Explicit origin allowlist from `CORS_ALLOWED_ORIGINS`. Never wildcard-with-credentials: the web app
authenticates with a header rather than cookies, so `allow_credentials` is never needed, and an
explicit list keeps a hostile page from scripting the API with a user's token.

**Methods and headers are mirrored from the preflight, not listed.** The origin allowlist is the
boundary; a hand-kept method list is a second copy of what the router serves, and it drifted — the
vault write is a `PUT`, `PUT` was missing, and the operator saw "Can't reach the API" rather than
anything naming a method or a route. A method the router does not serve now gets a `405` with a
body, which is readable; a preflight failure is not. `crates/wheel-api/tests/cors.rs`
(**API-cors-covers-every-served-method**) reads the routes out of the router's own source and holds
the preflight to every method each one accepts, so a return to a static list fails CI.

`ANY /p/{project_id}/{*rest}` carries its own permissive CORS (`Access-Control-Allow-Origin: *`, no
credentials): an ingress URL is public by definition, so restricting which page may read the reply
protects nothing and only stops the board's own "test this endpoint" button from showing it.

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
WHEEL_SIGNUP=open CORS_ALLOWED_ORIGINS=http://localhost:3000 ./target/debug/wheel-api
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
