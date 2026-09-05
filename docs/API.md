# `api.wheel.dev` — public API

Stateless gateway in front of `wheel-host`. Owned by the API team. Contract: `ARCHITECTURE.md` §5.

The API never talks to a container runtime and never talks to an engine directly. Every sandbox
operation is an authenticated call to the single `wheel-host` machine over private networking.

## Authentication

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
| `DATABASE_URL` | yes | — | Postgres. |
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

### The dev-bypass interlock

`AUTH_DEV_SECRET` accepts HS256 tokens, which anyone holding the secret can mint for any `sub`. It
is a complete authentication bypass, deliberately, for local testing.

**If it is set while `WHEEL_ENV` is not `dev`, the process refuses to start.** An unset `WHEEL_ENV`
counts as prod, because in practice nobody sets `WHEEL_ENV=prod` by hand — they just don't set it,
and that must not be the permissive case. Covered by `tests/config_interlock.rs`.

## CORS

Explicit origin allowlist from `CORS_ALLOWED_ORIGINS`. Never wildcard-with-credentials: the web app
authenticates with a header rather than cookies, so `allow_credentials` is never needed, and an
explicit list keeps a hostile page from scripting the API with a user's token.

## Local development

```bash
export API_MASTER_KEY=$(openssl rand -base64 32)
export CLERK_JWKS_URL=... CLERK_ISSUER=...
docker compose -f infra/docker-compose.yml up --build
```

The docker socket is mounted into the **host** service only. Anything that can reach the socket can
trivially escape to the machine, so the internet-facing API must never see it.
