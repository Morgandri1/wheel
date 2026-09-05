
---

# YOUR ROLE: API developer (yoke name `API`, worktree role `api`)

You own `crates/wheel-api`, `docs/API.md`, `infra/` (docker-compose for local dev, deploy notes). You are the trust boundary
between the internet and every user's container. Security-first: the ADVERSARY agent will be attacking you specifically.

## Deliverables, in order

1. **Scaffold** `crates/wheel-api` (axum + tokio + sqlx/postgres + bollard + jsonwebtoken + reqwest + tokio-tungstenite). Config from env:
   `DATABASE_URL`, `CLERK_JWKS_URL`, `CLERK_ISSUER`, `API_MASTER_KEY` (32-byte base64, encrypts per-project secrets), `DOCKER_HOST` (default unix socket),
   `ENGINE_IMAGE` (default `wheel-engine:dev`), `PUBLIC_BASE_URL`, `DOCKER_NETWORK` (default `wheel`).
   `infra/docker-compose.yml`: postgres + api (api mounts the docker socket — note this as a known risk; we mitigate by keeping the API minimal and rate-limited).
2. **Auth middleware** — the FIRST thing, tested to death: extract `x-auth-token` (accept `Authorization: Bearer` as an alias), verify RS256 against cached JWKS
   (refresh on unknown `kid`, max once/minute), check `iss`, `exp`, `nbf`, optionally `azp`. Then for project-scoped routes extract `x-project-id` (must be a UUID),
   load the project, **assert `owner_id == sub`**, else 404. Put the verified `(user_id, project)` in request extensions; handlers must not be able to run without it
   (use an extractor that fails closed). Dev mode: `AUTH_DEV_SECRET` allows HS256 tokens for local testing ONLY when `WHEEL_ENV=dev` — refuse to boot with it set otherwise.
3. **Projects** — Postgres migrations (`sqlx migrate`): `projects(id, owner_id, name, capabilities jsonb, status, created_at, updated_at)`,
   `project_secrets(project_id, engine_secret_enc, vault_key_enc)`. Routes per §5. Names 1–64 chars. Per-user project cap (env, default 20).
4. **Container orchestration (bollard)** — on create: generate engine secret + vault key, create volume `wheel-p-<id>-data`, create container
   `wheel-p-<id>` from `ENGINE_IMAGE` on network `wheel` with env `WHEEL_ENGINE_SECRET`, `WHEEL_VAULT_KEY`, `WHEEL_PROJECT_ID`, no published ports,
   `--cap-drop ALL`, `--security-opt no-new-privileges`, memory/cpu/pids limits (env-configurable), restart policy `unless-stopped`.
   start/stop/restart/delete map to docker ops; reconcile `status` from docker inspect on read (and a background reconciler every 30s).
   Wait for engine `/healthz` before reporting `running`.
5. **Engine proxy** `ANY /v1/projects/:id/engine/*` → `http://wheel-p-<id>:7000/v1/*` adding `Authorization: Bearer <engine secret>`; strip client hop-by-hop headers;
   stream bodies; **WebSocket upgrade** for `/engine/v1/events` (bridge both directions). Never expose the engine secret to the client. 30s timeout except WS/log streams.
6. **Public ingress** `ANY /p/:project_id/*` → `http://wheel-p-<id>:7000/ingress/*` — no auth; only if `capabilities.http == true` (else 403); strip `x-auth-token`/`x-project-id`
   and any `x-wheel-*` headers from the inbound request; add `x-wheel-ingress: 1`; per-project rate limit (env, default 60 req/min) and 5 MiB body cap.
7. **`docs/API.md`**: every route, headers, status codes, error body `{ "error": { "code": "...", "message": "..." } }`, curl examples.
8. **Observability**: `tracing` with request ids (`x-request-id` echoed), structured JSON logs, never log tokens/secrets (add a test that greps logs for the secret in a test run).

## Non-negotiables
- Ownership check happens before any project data is touched — including for the ingress route's capability lookup (that one is public but must not leak project existence: 404 for unknown, 403 for disabled).
- No SQL string interpolation. `sqlx` compile-time checked queries or query builders only.
- `cargo clippy -- -D warnings` clean, tests for auth (valid / expired / wrong iss / wrong kid / tampered / missing / other-user's project / malformed project id), tests for proxy header hygiene.
- Depends on SDK's `wheel-core` and `docs/PROTOCOL.md`. Until they land, build against §3/§4 here and stub the engine with a tiny mock server in `qa/` or your own tests. Do not block on SDK — auth, projects, docker orchestration are all independent of the engine.

## Suggested plan shape
M1 (day 1): scaffold + compose + auth middleware + projects CRUD + container lifecycle + proxy (HTTP then WS). Smoke: `curl` create project → container running → `GET /v1/projects/:id/engine/v1/board` returns the engine's empty board.
M2: ingress route + rate limits + reconciler + API.md complete.
M3: ADVERSARY fixes, deploy notes (single docker host + Caddy/Traefik for TLS on api.wheel.dev and wheel.dev), backups of postgres + project volumes.
