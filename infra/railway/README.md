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
* **A health check must point at a port the platform actually probes.** `wheel-host` used to bind
  `0.0.0.0:7100` and ignore `$PORT`, so every probe reached nothing and answered "service
  unavailable" — whatever path it was pointed at. That took the service down twice: once with
  `/host/v1/healthz` (which is behind the `WHEEL_HOST_SECRET` bearer a checker cannot present, a
  real bug, fixed by adding an unauthenticated `/healthz`) and then again with `/healthz`, which
  failed identically. The second failure is what proved the cause was the port, not the path. The
  host now binds `$PORT` when the platform sets it, `PORT=7100` keeps `WHEEL_HOST_URL` working, and
  the check is back on `/healthz`.
* **The host serves before it reconciles.** Restoring fourteen projects takes longer than the 30s
  health-check window, and a host stopped for failing that check never reconciles at all. Liveness
  answers immediately; project routes return 503 `starting` until reconcile finishes, so nothing is
  ever served from a half-restored view.
* To remove a health check, delete the key from `settings.json` rather than setting it to `null`:
  Railway's API accepts a null and ignores it, reporting success while the old path stays in force.
* Setting a health check on a service whose replica is already stopped does not fail loudly — the
  deploy just never goes healthy. Check `railway logs -s <service> --build` for "Starting
  Healthcheck" if a deploy is FAILED with a green build.

## Environment

Set in the Railway dashboard or with `railway variables --set`, never in git.

`wheel-api`: `DATABASE_URL`, `API_MASTER_KEY`, `AUTH_MODE`, `WHEEL_ENV`, `WHEEL_HOST_URL`,
`WHEEL_HOST_SECRET`, `PUBLIC_BASE_URL`, `CORS_ALLOWED_ORIGINS` (comma-separated; currently
`https://wheel.dev,https://www.wheel.dev,https://wheel-2708.vercel.app`).

`wheel-host`: `WHEEL_HOST_SECRET` (same value as the API's), `SANDBOX_BACKEND=process`,
`WHEEL_DATA_DIR=/data`, volume mounted at `/data`.

## Pruning probe projects

Automated probes create projects on production and mostly clean up after themselves; the ones that
do not each keep an engine resident on the single host, which costs memory and slows the next
person's project creation (0.71 s with fourteen engines up, 0.49 s with one).

```bash
DATABASE_URL=… ./infra/prune-probe-projects.sh              # list candidates, delete nothing
DATABASE_URL=… WHEEL_HOST_URL=… WHEEL_HOST_SECRET=… \
  ./infra/prune-probe-projects.sh --apply
```

It is run by hand, not on a schedule. A project is a candidate only if it is not on the deny list
(the operator's own account, and the `wheel-dev` board), its owner's address is at `wheel.test`,
`wheelcheck.dev` or `example.com` exactly, and it is more than 24 hours old. The sandbox is
destroyed through the host before the row is dropped, so nothing is left running on the host with no
record of it.

Predicates are covered by `infra/tests/prune-probe-projects.test.sh`, and a `psql` that cannot
connect aborts the run rather than reporting "0 projects, 0 candidates" — the one output that looks
like a clean bill of health.

**Where to run it.** It needs `DATABASE_URL`, and Postgres has no public proxy: the URL Railway
hands out resolves only inside the project's private network, so the script cannot reach it from a
laptop. Run it from a container that can:

```bash
railway ssh --service Postgres    # psql is on that image; DATABASE_URL resolves there
```

Enabling a public Postgres proxy would make it runnable from anywhere and is the operator's call,
not something to turn on for a cleanup script.

## The volume

`wheel-host` mounts a 5 GB Railway volume at `/data`. **Railway's dashboard figure for it is not
reliable** — on 6 Sep it read 127 MB while `df -h /data` inside the container read 4.5 G of 4.6 G,
100 % used. Check it the honest way:

```bash
railway ssh --service wheel-host "df -h /data && du -sh /data/projects/*/* | sort -h | tail"
```

A full volume is not a quiet failure. sqlite reports it as `disk I/O error ... trying to resize an
existing shared-memory segment` while growing a WAL index, which reads like a filesystem
incompatibility and is not one: it is ENOSPC. That cost ninety minutes of downtime and two wrong
fixes. So the host now measures it itself — free space is logged at boot, reported on
`GET /host/v1/healthz` as `disk_free_mb` / `disk_used_percent`, and a project start below
`DISK_FLOOR_MB` (default 256) is refused with `507` and a message that names the disk.

What filled it was per-agent Rust toolchains: each agent node had its own `.rustup` (1.1–1.5 GB)
inside its private credentials directory, and one had cloned the repo into that directory and built
it there (a 1.9 GB `target/`). ARCHITECTURE M1.6 wants `CARGO_HOME`/`RUSTUP_HOME` **per project**;
the engine's spawn environment is SDK's.
