# 048 — deploy the §5b network segmentation (wheel-host into its own Railway project)

**Status:** proposal, not yet executed. Loop adversary in before anyone runs the runbook (PM's
instruction, and the right call independent of it — this touches live tenant data on a volume that
cannot be trivially restored).

**Owner:** API. **Gates:** `docs/proposals/agent-grid-engine.md` §4 names this, alongside 037, as the
explicit security gate before real hosted multi-tenant traffic. **Confirmed live** by PM
(`redteam/findings/048-s5b-network-isolation-not-deployed.md`): `postgres.railway.internal:5432` and
`wheel-api.railway.internal:8080` both accept TCP from the wheel-host container today.

## Why this needs a plan before it needs a command

Moving `wheel-host` to a new Railway project is not a settings change — it is a new service with a
new volume, and the OLD volume holds every tenant's real data (`host.db`'s sandbox table, and every
project's `/data/projects/<id>/…`: sqlite state, vault ciphertext, chest blobs, git workspaces,
per-project Rust toolchains). There is no Railway "move project" operation; the data has to be copied,
and until the copy is verified, the old volume is the only copy that exists. That makes this
hard-to-reverse in the sense the operating instructions mean it, independent of severity — get the
cutover order wrong and it is a real outage or real data loss, not a config typo to fix forward.

**I do not have Railway credentials in this sandbox** (`railway` CLI is not installed here, and the
`secrets` vault this project wires me to declares no keys — confirmed, not assumed, before writing
this). `infra/railway/apply-settings.sh` already documents its own prerequisite as "auth comes from
the Railway CLI's own session, so `railway login` is the only prerequisite" — i.e. this has always been
run by whoever is logged in locally, not by CI or an agent. So this doc is the runbook for whoever holds
that session (operator), not something I can execute end-to-end myself. What I *can* and did do:
prepare every config/script change the migration needs, so the runbook is "run these," not "figure out
what to run."

## Target topology (§5b, verbatim)

> `wheel-host` … **in its OWN Railway project** (separate private network from the API + Postgres —
> ADVERSARY finding 003) … Reached by the API over its Railway-issued HTTPS domain with
> `WHEEL_HOST_SECRET` bearer + TLS (private networking does not cross projects) … Agents inside
> sandboxes therefore cannot reach Postgres or API internals at all.

Two Railway projects instead of one:

| Project | Services | Reachability |
|---|---|---|
| `wheel` (existing, `c6049c66-…`) | `wheel-api` (2 replicas), `Postgres` | Private networking between these two, as today. No `wheel-host`. |
| `wheel-host` (new) | `wheel-host` (1 replica), its own volume | No private link to the `wheel` project. Reached from the API over a **public HTTPS domain, bearer-gated** — the only channel Railway offers across projects. |

This means `wheel-host` **will** have `RAILWAY_PUBLIC_DOMAIN` set, which is the opposite of today's
guard (`crates/wheel-host/src/config.rs:112-121` refuses to boot with one, unless
`ALLOW_PUBLIC_DOMAIN=1`). That guard's comment ("the host must never be reachable from the internet")
described the *old* topology, where private-networking-only was the whole protection; under §5b's
actual target, bearer+TLS on a public domain is the protection, by design — that is what "reached over
its Railway-issued HTTPS domain" means. The escape hatch already exists for exactly this
(`ALLOW_PUBLIC_DOMAIN=1`); nothing in `wheel-host`'s own code needs to change for the target state,
but the comment is now misleading about *why* and should be corrected in the same commit that flips the
flag in production, so a future reader doesn't "fix" it back.

## What breaks and needs updating

1. **`infra/railway/settings.json`** — one `project`/`environment` pair today, shared by both
   services. Needs a per-service project so the two live in different Railway projects. Done in this
   branch (see diff): each service now carries its own `project`/`environment`, defaulting to the
   shared top-level value so nothing else has to change until the new project id exists.
2. **`WHEEL_HOST_URL`** (on `wheel-api`) — today presumably `http://wheel-host.railway.internal:7100`
   (private DNS, same project). Once `wheel-host` moves, that name will not resolve from the `wheel`
   project at all — this is the fix taking effect, not a bug. Must become
   `https://<wheel-host's-new-public-domain>` before cutover, or the API loses its host entirely.
3. **`infra/prune-probe-projects.railway.sh`** — reaches `wheel-host` via `railway ssh --service
   wheel-host` today, which only works because the operator's local Railway session can address a
   service by name within one linked project. Once `wheel-host` is a separate project, this script's
   `railway link` step needs to target the `wheel-host` project specifically for that half of the
   script (it already separately links for Postgres) — a `railway link -p wheel-host` before the
   `wheel-host` calls, not a code change to the reviewed prune script itself.
4. **A deploy check that asserts the isolation, not just sets it up once** (finding 048's own fix
   recommendation) — added in this branch: `infra/railway/verify-network-isolation.sh`. Run after
   cutover and periodically after: SSHes into `wheel-host` and asserts `postgres.railway.internal` and
   `wheel-api.railway.internal` do NOT resolve/connect. Non-zero exit if either does, so it is fit for
   a CI/cron check, not just a one-time manual assertion.

## Migration runbook (for whoever runs this — needs Railway dashboard + CLI access)

This is a maintenance window: `wheel-host` is the single supervisor for every tenant engine, so every
project on the platform is unreachable for the duration of the volume copy. Say so to whoever's
projects are affected before starting, not after.

1. **Freeze writes.** Stop the `wheel-host` service (Railway dashboard or `railway down -s wheel-host`
   is NOT what we want — that removes the deployment; use the service's own stop/scale-to-zero, or
   simplest: `railway service delete` is destructive, so just pause traffic by scaling replicas to 0
   via the dashboard). Confirm `railway logs -s wheel-host` shows it stopped, not crashlooping.
2. **Create the new project.** `railway init` (or the dashboard) → project name `wheel-host`. Note its
   project id and default environment id for `settings.json`.
3. **Add the service** in the new project: same repo (`Morgandri1/wheel`), same
   `docker/Dockerfile.host`, same watch paths as today's `wheel-host` entry in `settings.json`. Do
   **not** set a domain yet.
4. **Add a volume** to the new service, mounted at `/data`, sized at least as large as
   `railway ssh -s wheel-host "df -h /data"` reports for the old one (5 GB today per the README; check
   current usage first, the dashboard figure for this has been wrong before — see README's "The
   volume" section).
5. **Copy the data**, old service → local → new service, while the old one is stopped so the copy is
   consistent:
   ```bash
   railway link -p wheel -s wheel-host          # old project
   railway ssh -s wheel-host "tar czf - -C / data" > /tmp/wheel-host-data.tar.gz
   railway link -p wheel-host -s wheel-host      # new project (same service name, different project)
   cat /tmp/wheel-host-data.tar.gz | railway ssh -s wheel-host "tar xzf - -C /"
   railway ssh -s wheel-host "df -h /data && du -sh /data/projects/*/* | sort -h | tail"
   ```
   Verify the `du` output against what the old volume reported before deleting anything. Keep
   `/tmp/wheel-host-data.tar.gz` until the new host has been running cleanly for a while — it is the
   only rollback path if the new deployment misbehaves.
6. **Set environment on the new service**: same `WHEEL_HOST_SECRET` value as before (the API's copy
   must keep matching it), `SANDBOX_BACKEND=process`, `WHEEL_DATA_DIR=/data`,
   `WHEEL_HARNESS_AUTH_OAUTH_PROJECTS` (copy verbatim), and `ALLOW_PUBLIC_DOMAIN=1` — deliberately, per
   the guard above.
7. **Apply service settings** from the updated `infra/railway/settings.json` (`./apply-settings.sh`,
   once its `wheel-host` entry's `project`/`serviceId` point at the new project/service).
8. **Add a public domain** to the new `wheel-host` service (Railway dashboard → the only way to reach
   it now that it has no private link to `wheel-api`). Confirm it boots — `ALLOW_PUBLIC_DOMAIN=1` means
   it will not refuse itself, but confirm `GET https://<domain>/healthz` actually answers.
9. **Point the API at it**: `railway variables --set WHEEL_HOST_URL=https://<domain> -s wheel-api -p
   wheel`. Redeploy or wait for the running replicas to pick it up (check how `WHEEL_HOST_URL` is
   read — if it's read once at API boot, this needs a restart, not just a variable set).
10. **Verify end to end** before declaring done: create a throwaway project through the public API,
    confirm its engine starts, confirms a board round-trip, then delete it. Then run
    `infra/railway/verify-network-isolation.sh` and confirm it reports isolation, not just that the
    host is up.
11. **Decommission the old volume** only after the new one has run under real traffic for a period
    everyone is comfortable with (this is a judgement call for whoever runs this, not a fixed number —
    the tar backup is cheap insurance until then).

## What does NOT change

- `wheel-api` stays where it is, with Postgres, exactly as §5b already specifies for that pair.
- The host's own code (`crates/wheel-host`) needs no changes for this — `ALLOW_PUBLIC_DOMAIN=1` is an
  existing, already-tested escape hatch (`crates/wheel-host/tests/config_and_proxy.rs`), not new
  surface. Only the doc comment explaining *why* the guard exists should be tightened in the same
  commit that flips it, so it states the target reasoning rather than the old one.
- `WHEEL_HOST_SECRET` stays the single bearer between the two projects — nothing here changes what
  authenticates the API↔host hop, only which network it travels over.

## Open questions for review (adversary + PM)

1. Is bearer+TLS over a public domain, with no other network control, an acceptable final posture for
   `wheel-host`, or does this need to also land alongside rate limiting / IP allowlisting on that
   domain given it fronts the sandbox supervisor for every tenant? §5b's own text treats bearer+TLS as
   sufficient; flagging rather than assuming that still holds now that it's a real deploy, not a
   design doc.
2. The runbook above is a real maintenance window (every tenant engine down during the volume copy).
   Confirm that's acceptable, or whether a live-migration approach (stand up the new host, replicate
   ongoing writes, cut over with a much shorter freeze) is worth the extra complexity for this one
   move. I think the simple stop-copy-start version is right for a one-time migration of the current
   data size (single-digit GB), but this is exactly the kind of judgement call that should not be made
   unilaterally.
3. Confirm who actually runs the runbook — this doc assumes "whoever holds the Railway session," which
   today is the operator. If that's meant to become an agent's job going forward, that agent needs a
   scoped Railway credential handed to it deliberately (the credential-distribution rule in
   ARCHITECTURE.md is written for a different credential class, but the spirit — no change to a
   dangerous distribution path without review recorded in `redteam/reviews/` — applies here too).
