# Railway deployment

Two services in the Railway project `wheel` (workspace "Morgan Metz's Projects"), plus Postgres.
Both are connected to `github.com/Morgandri1/wheel`, branch `main`, and **deploy on push**.

| Service      | Dockerfile               | Replicas | Healthcheck          | Domain |
|--------------|--------------------------|----------|----------------------|--------|
| `wheel-api`  | `docker/Dockerfile.api`  | 2        | `/healthz`           | `wheel-api-production.up.railway.app` |
| `wheel-host` | `docker/Dockerfile.host` | **1**    | `/healthz`           | none — private only |

`wheel-host` must stay at one replica. It owns per-project sandboxes and a sqlite state file on a
Railway volume; a second replica would fight it for both, and two supervisors reconciling the same
projects would start and stop each other's engines.

## How a deploy happens

Push to `main`. Railway builds that commit from the repo — nothing is uploaded from a laptop, so
what runs in production is always a commit that exists in git history and passed CI.

Each service only rebuilds when a path it actually depends on changes (Railway "watch paths"), so an
API-only change does not rebuild the host and vice versa:

* `wheel-api` — `crates/wheel-api/**`, `crates/wheel-core/**`, `Cargo.toml`, `Cargo.lock`, `docker/Dockerfile.api`
* `wheel-host` — `crates/wheel-host/**`, `crates/wheel-engine/**`, `crates/wheel-cli/**`, `crates/wheel-core/**`, `Cargo.toml`, `Cargo.lock`, `docker/Dockerfile.host`, `docker/entrypoint.sh`

Confirm what actually shipped — the answer includes the commit, so a stale deploy is visible rather
than assumed:

```bash
railway link -w "Morgan Metz's Projects" -p wheel -e production -s wheel-api   # -s FIRST; see below
railway deployment list -s wheel-api
```

`railway up` uploads the working tree instead, bypassing git and CI. It is for emergencies only, and
whatever it deploys must be pushed to `main` immediately afterwards or the next GitHub deploy will
silently revert it.

## Why there is no railway.toml

Railway **deprecated** config-as-code (`railway.json` / `railway.toml`) in favour of
Infrastructure-as-Code (`.railway/railway.ts`, which requires the Railway TypeScript SDK as a repo
dependency). The API now rejects the setting outright:

> Config as Code (railway.json / railway.toml) is deprecated. Use Infrastructure as Code
> (.railway/railway.ts) instead.

The `infra/railway/{api,host}/railway.toml` files were therefore never read by Railway — the services
were on the default builder the whole time — so they were deleted rather than left looking live. A
file that looks like configuration and is not is worse than no file.

Service settings are applied with `./apply-settings.sh`, which reads `settings.json` and is
idempotent: run it after any change there, and to re-assert settings if someone edits them by hand.
Adopting `.railway/railway.ts` is the eventual path; it is not worth a root-level node dependency
today.

## Gotchas

* **`railway link -s <service>` first.** `railway volume add` and `railway domain` ignore
  `--service` and act on whatever is linked. This has already put a volume on the wrong service and
  created a public domain on `wheel-host`, which must never have one.
* `wheel-host` refuses to boot if `RAILWAY_PUBLIC_DOMAIN` is set (override: `ALLOW_PUBLIC_DOMAIN=1`),
  so that mistake fails loudly instead of silently exposing every tenant's sandbox supervisor.
* Postgres has no public proxy. To query production, `railway ssh -s postgres` and use `psql` there.
* A health check must point at an endpoint the platform can actually reach. `wheel-host` puts every
  `/host/v1/*` route behind the `WHEEL_HOST_SECRET` bearer, which a health checker cannot present:
  pointing the check at `/host/v1/healthz` made every probe 401, Railway stopped the container, and
  every project create hung on an unreachable host. Use the unauthenticated `/healthz`, which
  reports liveness and nothing else.

## Environment

Set in the Railway dashboard or with `railway variables --set`, never in git.

`wheel-api`: `DATABASE_URL`, `API_MASTER_KEY`, `AUTH_MODE`, `WHEEL_ENV`, `WHEEL_HOST_URL`,
`WHEEL_HOST_SECRET`, `PUBLIC_BASE_URL`, `CORS_ALLOWED_ORIGINS` (comma-separated; currently
`https://wheel.dev,https://www.wheel.dev,https://wheel-2708.vercel.app`).

`wheel-host`: `WHEEL_HOST_SECRET` (same value as the API's), `SANDBOX_BACKEND=process`,
`WHEEL_DATA_DIR=/data`, volume mounted at `/data`.
