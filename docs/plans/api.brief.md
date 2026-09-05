
---

# YOUR ROLE: API developer (yoke name `API`, worktree role `api`)

You own `crates/wheel-api`, `docs/API.md`, `infra/` (docker-compose for local dev, Railway config, deploy notes). The API is a **stateless,
horizontally-scaled gateway** (Railway, N replicas) in front of the single `wheel-host` machine. You are the trust boundary between the internet and every user's sandbox. Security-first: the ADVERSARY agent will be attacking you specifically.

## Deliverables, in order

1. **Scaffold** `crates/wheel-api` (axum + tokio + sqlx/postgres + jsonwebtoken + reqwest + tokio-tungstenite). Config from env:
   `DATABASE_URL`, `CLERK_JWKS_URL`, `CLERK_ISSUER`, `API_MASTER_KEY` (32-byte base64, encrypts per-project secrets), `WHEEL_HOST_URL`
   (e.g. `http://wheel-host.railway.internal:7100`), `WHEEL_HOST_SECRET`, `PUBLIC_BASE_URL`, `WHEEL_ENV`.
   `infra/docker-compose.yml`: postgres + api + host (host image from `docker/Dockerfile.host`, `SANDBOX_BACKEND=docker`, docker socket mounted into the HOST only — the API never sees docker).
   `infra/railway/{api,host}/railway.toml` for deploy; `docker/Dockerfile.api`.
2. **Auth middleware** — the FIRST thing, tested to death: extract `x-auth-token` (accept `Authorization: Bearer` as an alias), verify RS256 against cached JWKS
   (refresh on unknown `kid`, max once/minute), check `iss`, `exp`, `nbf`, optionally `azp`. Then for project-scoped routes extract `x-project-id` (must be a UUID),
   load the project, **assert `owner_id == sub`**, else 404. Put the verified `(user_id, project)` in request extensions; handlers must not be able to run without it
   (use an extractor that fails closed). Dev mode: `AUTH_DEV_SECRET` allows HS256 tokens for local testing ONLY when `WHEEL_ENV=dev` — refuse to boot with it set otherwise.
3. **Projects** — Postgres migrations (`sqlx migrate`): `projects(id, owner_id, name, capabilities jsonb, status, created_at, updated_at)`,
   `project_secrets(project_id, engine_secret_enc, vault_key_enc)`. Routes per §5. Names 1–64 chars. Per-user project cap (env, default 20).
4. **Host client** — on create: generate engine secret + vault key (store encrypted), `PUT /host/v1/projects/:id {engine_secret, vault_key, capabilities}`,
   then `POST …/start`. start/stop/restart/delete map 1:1 to host API calls (§4b); `status` comes from `GET /host/v1/projects/:id` on read, cached ≤5s.
   Host unreachable → project `status: "error"` with a clear error body, never a 500 stack trace. Retries with jittered backoff for idempotent calls only.
5. **Engine proxy** `ANY /v1/projects/:id/engine/*` → `WHEEL_HOST_URL/host/v1/projects/:id/engine/*` adding `Authorization: Bearer <host secret>`; strip client hop-by-hop headers;
   stream bodies; **WebSocket upgrade** for `/engine/v1/events` (bridge both directions). Never expose the host secret to the client. 30s timeout except WS/log streams.
6. **Public ingress** `ANY /p/:project_id/*` → `WHEEL_HOST_URL/host/v1/projects/:id/ingress/*` — no auth; only if `capabilities.http == true` (else 403); strip `x-auth-token`/`x-project-id`
   and any `x-wheel-*` headers from the inbound request; add `x-wheel-ingress: 1`; per-project rate limit (env, default 60 req/min) and 5 MiB body cap.
7. **`docs/API.md`**: every route, headers, status codes, error body `{ "error": { "code": "...", "message": "..." } }`, curl examples.
8. **Observability**: `tracing` with request ids (`x-request-id` echoed), structured JSON logs, never log tokens/secrets (add a test that greps logs for the secret in a test run).

## Non-negotiables
- Ownership check happens before any project data is touched — including for the ingress route's capability lookup (that one is public but must not leak project existence: 404 for unknown, 403 for disabled).
- No SQL string interpolation. `sqlx` compile-time checked queries or query builders only.
- `cargo clippy -- -D warnings` clean, tests for auth (valid / expired / wrong iss / wrong kid / tampered / missing / other-user's project / malformed project id), tests for proxy header hygiene.
- Must be safe to run as N replicas: no in-memory state that matters. Depends on SDK's `wheel-core` and `docs/PROTOCOL.md`. Until they land, build against §3/§4/§4b here and stub the HOST with a tiny mock server in your tests. Do not block on SDK — auth, projects, host client are all independent of the engine.

## Suggested plan shape
M1 (day 1): scaffold + compose + auth middleware + projects CRUD + host client + proxy (HTTP then WS). Smoke: `curl` create project → sandbox running → `GET /v1/projects/:id/engine/v1/board` returns the engine's empty board.
M2: ingress route + rate limits + reconciler + API.md complete.
M3: ADVERSARY fixes, Railway deploy (api ×N replicas + host ×1 with volume + Railway Postgres; private networking; `api.wheel.dev` domain), backups of postgres + host volume.
