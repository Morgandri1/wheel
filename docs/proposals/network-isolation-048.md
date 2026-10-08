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

## Blocker found while writing this: the host's brute-force limiter breaks under a public domain

`wheel-host`'s `auth_limit` keys failed-bearer attempts on the **TCP peer address** and, once a peer
is over budget, refuses ALL its requests *before* comparing the bearer — including a correct one
(`crates/wheel-host/src/lib.rs` `require_bearer`; `auth_limit.rs`). Its own doc comment justifies
per-peer keying with "a hostile sandbox can only exhaust its own" — true on a private network, false
behind Railway's edge proxy, where the peer for every internet caller AND for `wheel-api` is the same
edge address (nothing reads `X-Forwarded-For`). So the moment the host gets a public domain, anyone
who learns the domain can burn the failure budget for that shared peer and lock `wheel-api` out of the
host: platform-wide outage from one `curl` loop, no secret needed.

**This must be fixed before the migration, not after** — and it changes what the limiter is (adversary, #138):
a limiter can only throttle guesses by refusing them *without evaluating them*; once a correct bearer must always
pass, every guess is evaluated and a right one returns 200 whatever the budget. Availability and guess-bounding are
incompatible at the limiter. So after the fix it is a logging/alerting device, and **the only real guess resistance
is the secret's entropy.** Prerequisites, all in the limiter PR (#140) or beside it:
1. Always admit a correct bearer. (Done in #140.)
2. Charge failures to the edge-observed client address (`WHEEL_TRUSTED_PROXIES`). Defence in depth only. Whether
   Railway's edge appends or passes a client `X-Forwarded-For` through is **unverified**; before relying on it, run
   `curl -H 'X-Forwarded-For: 203.0.113.9' https://<domain>/host/v1/healthz` against a build that logs the header, and
   record the answer here.
3. **Bound the failure map**: sweep expired entries on insert and cap the map (overflow shares one bucket), with a
   test that 10^5 distinct keys stay under the cap — otherwise an unauthenticated caller rotating a header value grows
   it without limit and OOMs the supervisor of every tenant.
4. **Enforce entropy under a public domain**: with `ALLOW_PUBLIC_DOMAIN=1`, refuse to boot on a `WHEEL_HOST_SECRET`
   under 32 characters (today 16 is accepted).
5. **Two accepted secrets (current + previous)** so rotation is not a synchronized redeploy of the host and every API
   replica — otherwise rotation will simply never happen.
6. **Tighten the guard, not just the flag**: under `ALLOW_PUBLIC_DOMAIN=1` also require `RAILWAY_PUBLIC_DOMAIN` to equal
   a configured `WHEEL_HOST_EXPECTED_DOMAIN`, so a second accidental `railway domain` still stops the boot.
7. Alert on the host's 401/429 rate (the log line exists).

What bearer+TLS on a public domain does and does not buy: sandboxes lose the private route to Postgres and to
`wheel-api.railway.internal:8080` (which also matters for #136's trusted-proxy header). It does NOT stop a sandboxed
agent, which has internet egress, from reaching the host's public domain; one shared secret still guards every tenant.

## Migration checklist (operator; needs the Railway session — `railway login`, workspace "Morgan Metz's Projects")

Maintenance window: every tenant engine is down from step 3 to step 12. Announce first. Nothing below
deletes the old service or its volume; until step 15 the old volume is an untouched, complete copy.
Do NOT let the old `wheel-host` run again after cutover (two hosts would both start every tenant's
agents); rollback means re-pointing `WHEEL_HOST_URL`, not restarting it (see Rollback).

Prerequisite: the limiter PR above is merged and deployed to the OLD host and the new image builds.

**A. Prepare (no downtime)**
1. [ ] Record the baseline: `railway link -w "Morgan Metz's Projects" -p wheel -e production -s wheel-host`, then
   `railway ssh -s wheel-host 'df -h /data; ls /data/projects | wc -l'` and
   `railway ssh -s wheel-host 'curl -s -H "Authorization: Bearer $WHEEL_HOST_SECRET" localhost:${PORT:-7100}/host/v1/healthz'`
   (note `projects_running`). Have ≥ that much free disk locally.
2. [ ] Create the project: `railway init -n wheel-host -w "Morgan Metz's Projects"` (or dashboard → New Project → Empty).
   Note the new **project id**, **environment id** and (after step 4) **service id** — they go in
   `infra/railway/settings.json` under `wheel-host` as `"project"`, `"environment"`, `"serviceId"`.
3. [ ] In the new project add a service from GitHub `Morgandri1/wheel`, branch `main`, Dockerfile
   `docker/Dockerfile.host` (dashboard: New → GitHub Repo). **Cancel/skip its first deploy, or let it
   fail** — it must not start against an empty volume. Do not add a domain.
4. [ ] `railway link -p wheel-host -e production -s wheel-host` (link the NEW project; `-s` first, see README
   "Gotchas"), then `railway volume add -m /data`. Size ≥ the old volume's used space + headroom
   (old is 5 GB; measured 4.5 G used on 6 Sep — check step 1's `df`, don't trust the dashboard).
5. [ ] Set variables on the NEW service (`railway variables --set 'K=V' -s wheel-host`, never paste the
   secret into chat/git, and do not put it on a command line — it lands in shell history and argv; use the dashboard
   variable editor or stdin). Read the old values with `railway variables -s wheel-host --kv` while linked
   to the OLD project. Copy verbatim: `SANDBOX_BACKEND=process`, `WHEEL_DATA_DIR=/data`,
   `WHEEL_HARNESS_AUTH_OAUTH_PROJECTS`, `PORT=7100`, plus any others `--kv` shows.
   **New/changed:** `ALLOW_PUBLIC_DOMAIN=1` (must be set BEFORE any domain exists or the host refuses to
   boot); `WHEEL_HOST_SECRET` = a **fresh** `openssl rand -hex 32` — the host is about to be
   internet-reachable, so do not carry the old 16+-char secret across; set the same value on `wheel-api`
   in step 13.
6. [ ] Park the new service so restoring data doesn't race a running host: in the dashboard, new service →
   Settings → Deploy: set **Custom Start Command** `sleep infinity` and clear the healthcheck path
   (a failing check fails the deploy), then deploy. Confirm `railway ssh -s wheel-host 'ps aux | head'` shows
   only `sleep`, and `df -h /data` shows the new empty volume.

**B. Cutover (downtime starts)**
7. [ ] Park the OLD host the same way: link the OLD project, `-s wheel-host`; dashboard → Settings → Deploy →
   Custom Start Command `sleep infinity`, clear healthcheck path, redeploy. Confirm no `wheel-host`
   process (`ps aux`) — this quiesces every engine and `host.db`, giving a consistent copy. (This is
   why we do not simply stop the service: `railway ssh` needs a running container to read the volume.)
8. [ ] Checksum on the old side: `railway ssh -s wheel-host 'tar --numeric-owner -czf - -C / data | sha256sum'`. Save it.
9. [ ] **Encrypt before it leaves the container.** The tarball holds every tenant's decrypted secrets (`host.db` stores
   `engine_secret` and `vault_key` as plaintext, beside the vault ciphertext). Generate a key locally and keep it out of
   the archive's path; the transfer is then, e.g. (key passed via the remote environment, never argv):
   `railway ssh -s wheel-host 'tar --numeric-owner -czf - -C / data | openssl enc -aes-256-cbc -pbkdf2 -pass env:K | base64 -w0' | tr -d '\r\n' | base64 -d > wheel-data.enc`
   (base64 because a tty in `railway ssh` corrupts raw binary; the prune script's `tr -d '\r'` is the same fix). Compare
   `sha256sum wheel-data.enc` with the same pipeline's hash computed remotely — must be equal; mismatch = stop.
10. [ ] Push it up and decrypt on the new volume: `base64 -w0 wheel-data.enc | railway ssh -s wheel-host 'base64 -d | openssl enc -d -aes-256-cbc -pbkdf2 -pass env:K | tar --numeric-owner -xzpf - -C /'`.
    **Unverified that `railway ssh` forwards stdin — test with a 1 MB file to `/data/.probe` first.** If it does not, the
    fallback is a private bucket + short-lived presigned URL carrying the ENCRYPTED file only; the object is deleted in step 15.
11. [ ] Verify the RESTORE, not just the transfer, on BOTH sides (old volume before, new volume after) and compare:
    `find /data -xdev -printf '%p %U:%G %m %y %s\n' | LC_ALL=C sort | sha256sum` — covers ownership, mode, type and size of
    every path (the step 9 hash proves only the transfer). Then `PRAGMA integrity_check` on `host.db` and every project's
    `wheel.db`: parking by redeploy may SIGKILL after the drain window, so treat every sqlite file as crash-recovered (WAL
    recovery is safe because `-wal`/`-shm` travel with the tree). `find /data -type s` first: sockets are not archived
    (`/run/wheel` is not on the volume; engines recreate theirs). **Go/no-go for step 14, decided before starting:** every
    project reconciles to its step-1 state and `projects_running` equals step 1's value.
12. [ ] Unpark the new host: clear the Custom Start Command, restore the healthcheck (`./infra/railway/apply-settings.sh`
    after filling `settings.json` — its `wheel-host` entry now needs the new `project`/`environment`/`serviceId`),
    redeploy. `railway logs -s wheel-host` should show reconcile restoring the projects; wait for
    `projects_running` = step 1's value (project routes answer 503 `starting` until then — README "Gotchas").
13. [ ] Give it its address and repoint the API: `railway domain -s wheel-host` (with the NEW project linked and
    `-s` first — README warns `railway domain` acts on whatever is linked), target port 7100. Then, linked to the
    `wheel` project: `railway variables --set 'WHEEL_HOST_URL=https://<new-domain>' --set 'WHEEL_HOST_SECRET=<new secret>' -s wheel-api`.
    The API reads `WHEEL_HOST_URL` once at boot (`config.rs`), so this redeploys both replicas — watch
    `railway deployment list -s wheel-api` reach SUCCESS.

**C. Verify (downtime ends)**
14. [ ] `curl -s https://<new-domain>/healthz` → 200; `curl -s -o /dev/null -w '%{http_code}' https://<new-domain>/host/v1/healthz` → **401**
    (bearer enforced on the public route). Through the public API: sign in, open an existing project,
    confirm its board loads and an agent responds; create a throwaway project and delete it.
    Then `railway link -p wheel-host -s wheel-host && ./infra/railway/verify-network-isolation.sh` → exit 0. **This is the acceptance test for 048.** It exits 0 (isolated, every probe ran), 1 (reachable — 048 open) or 2 (could not
    measure — never reported as isolated); it probes as root AND as a sandbox uid, with a public-name positive control, and is
    itself tested against a stub `railway` (`infra/tests/verify-network-isolation.test.sh`).
15. [ ] Leave the OLD service parked (`sleep infinity`) and its volume alone for a soak period (your call, days not
    hours). **Then, as a step not a suggestion: delete `wheel-data.enc` from the laptop, any bucket object, and the key.**
    Only then delete the old service + volume. Update
    `infra/prune-probe-projects.railway.sh` (its TODO) and README topology text in the same PR that fills in `settings.json`.

**Rollback (any point before step 15's deletion):** link `wheel`, `railway variables --set 'WHEEL_HOST_URL=http://wheel-host.railway.internal:7100' --set 'WHEEL_HOST_SECRET=<old secret>' -s wheel-api`;
park/stop the NEW service first, then un-park the OLD (clear start command, restore healthcheck, redeploy).
Data written on the new host after cutover is not on the old volume — decide before rolling back after real traffic.

## What does NOT change

- `wheel-api` stays where it is, with Postgres, exactly as §5b already specifies for that pair.
- The host's own code (`crates/wheel-host`) needs no changes for this — `ALLOW_PUBLIC_DOMAIN=1` is an
  existing, already-tested escape hatch (`crates/wheel-host/tests/config_and_proxy.rs`), not new
  surface. Only the doc comment explaining *why* the guard exists should be tightened in the same
  commit that flips it, so it states the target reasoning rather than the old one.
- `WHEEL_HOST_SECRET` stays the single bearer between the two projects — nothing here changes what
  authenticates the API↔host hop, only which network it travels over.

## Open questions for review (adversary + PM)

1. Bearer+TLS on a public domain: **not acceptable as-is** — see the limiter blocker above (a shared edge peer
   turns per-peer throttling into a lockout lever). With that fixed and a 32-byte secret, is there anything
   further you want on the domain (IP allowlist is not available on Railway's edge, so likely no)?
2. The checklist above is a real maintenance window (every tenant engine down during the volume copy).
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
