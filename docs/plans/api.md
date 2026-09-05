# API team plan (`crates/wheel-api`, `docs/API.md`, `infra/`)

Owner: API agent. Contract: `docs/ARCHITECTURE.md` §5, §4b, §5b. Brief: `docs/plans/api.brief.md`.
Also owns `crates/wheel-host` (sandbox supervisor), `docker/Dockerfile.api`, `infra/railway/{api,host}/`.
Status key: [ ] todo · [~] in progress · [x] done

## Guiding principle

`wheel-api` is the only thing between the public internet and every user's container. Everything else is
negotiable; the trust boundary is not. Concretely, three invariants that the code is structured to make
*unbreakable by a careless handler*, not merely observed by a careful one:

1. **A project-scoped handler cannot run without a verified owner.** The only way to name a project inside a
   handler is `ProjectScope`, an extractor that has already verified the JWT and asserted `owner_id == sub`.
   There is no code path that turns a raw `:id` path segment into a project row. Fail-closed by construction.
2. **The engine secret never leaves the process.** It is decrypted per-request, attached to the upstream
   `Authorization` header, and never rendered into a response, a log line, or an error.
3. **Unknown and unowned are indistinguishable.** Both are 404. No enumeration oracle.

## File layout

```
crates/wheel-api/
  Cargo.toml
  migrations/0001_init.sql        # projects, project_secrets
  src/
    main.rs                       # boot, fail-closed config validation, router, reconciler task
    config.rs                     # env parsing; refuses to boot on unsafe combinations
    error.rs                      # ApiError -> { "error": { "code", "message" } }; Display never carries secrets
    state.rs                      # AppState: pg pool, jwks cache, bollard, config, rate limiter
    crypto.rs                     # AES-256-GCM seal/open under API_MASTER_KEY
    auth/
      jwks.rs                     # JWKS cache, refresh-on-unknown-kid, throttled to 1/min
      claims.rs                   # iss/exp/nbf/azp validation
      extractor.rs                # AuthUser, ProjectScope  <- the two fail-closed extractors
    routes/{health,projects,proxy,ingress}.rs
    orchestrator/host.rs          # client for wheel-host; the API owns no container runtime
    http/{hop,ratelimit}.rs       # hop-by-hop header hygiene, per-project token bucket
  tests/                          # auth matrix, proxy hygiene, ingress, secret-never-logged
infra/docker-compose.yml          # postgres + api
docs/API.md
```

## Milestones

### M0 — plan [x]
### M1 — vertical slice
- [x] Scaffold crate, workspace member, config, error type, `/healthz`
- [x] **Auth middleware + the full negative test suite** — 26 tests green, incl. the four ADVERSARY named
- [x] Migrations + projects CRUD (name 1–64, per-user cap default 20)
- [x] Engine proxy (HTTP + WS bridge) and public ingress route
- [x] `infra/docker-compose.yml`, `docker/Dockerfile.api`, Railway configs
- [x] Database-backed integration tests (ownership 404s, cross-tenant routes, ingress gate)
- [x] `crates/wheel-host`: `Sandbox` trait + docker backend, host API on :7100, sqlite state, boot reconcile, WS bridge
- [ ] E2E against the live stack (`infra/dev/e2e.py`) — blocked only on the docker image build
- [x] Host client (PUT/start/stop/restart/DELETE/status), jittered retry on idempotent calls only
- [x] Engine proxy HTTP, then WS bridge for `/engine/v1/events` (both API→host and host→engine hops)
- [x] `infra/docker-compose.yml` + Railway configs + `docker/Dockerfile.api`

### M2 — public surface
- [ ] `/p/:project_id/*` ingress: capability gate, header scrubbing, rate limit, 5 MiB cap
- [ ] Background reconciler, `docs/API.md` complete, request-id tracing

### M3 — hardening
- [ ] ADVERSARY findings, deploy notes (single host + Caddy TLS), postgres + volume backups

## Auth test matrix (written alongside the middleware, not after)

valid · expired · not-yet-valid (`nbf`) · wrong `iss` · unknown `kid` · known `kid`/wrong signature ·
tampered payload · `alg: none` · **HS256 forged with the RSA public key as the HMAC secret** (the classic
confusion attack) · missing header · malformed bearer · other user's project (→404) · nonexistent project
(→404, byte-identical to the previous case) · malformed project uuid · dev HS256 token while `WHEEL_ENV != dev`
(→ must not even boot) · JWKS refresh throttle (two unknown kids in a minute = one fetch).

## Decisions I'm making inside my own area

- **`sqlx` runtime-bound queries, not the `query!` macros, for now.** The macros need a live database *at
  compile time*; that would make `cargo build` depend on a running postgres and break every other agent's
  `make check`. `query_as::<_, T>(...).bind(...)` is equally injection-proof — the non-negotiable is "no SQL
  string interpolation", which binding satisfies. I'll generate offline `.sqlx` metadata and switch to the
  macros once postgres is reliably up in CI, which gets the compile-time schema checking back without the
  build-time daemon dependency.
- **AES-256-GCM** for `project_secrets`, random 96-bit nonce prepended to the ciphertext.
- **Postgres fixed-window counter** for the ingress rate limit, not an in-memory bucket. The API runs as N
  replicas, and a per-replica bucket silently becomes N × the configured limit — the control weakens exactly
  as you scale. The contract permits per-replica limits in v1; this is cheap enough that it isn't worth the
  weaker guarantee. Tradeoff documented in API.md: a fixed window admits a 2× boundary burst.

## Risks / open questions

- **Docker socket exposure — largely resolved by the two-tier split.** The socket now lives only in
  `wheel-host`, which has no public domain; the internet-facing API never sees it. Residual risk moves to the
  host: anything that reaches its socket owns the machine, and all tenants share one kernel. Project ids are
  UUIDs from our own database and are never interpolated into image names, mounts, or commands.
- **`WHEEL_HOST_SECRET` must never appear in a sandbox's environment.** It is the only thing authenticating
  the API to the host, and sandboxes sit on the same private network. Flagged for ADVERSARY.
- **Process sandbox backend (M3)** needs ADVERSARY design review before I build it, per brief §4b.
- **Blocked-but-routed-around:** Docker daemon was not running on this host; I started OrbStack. `wheel-core`
  and `PROTOCOL.md` don't exist yet — per brief I build against ARCHITECTURE §3/§4 and mock the engine in
  tests rather than waiting on SDK.
