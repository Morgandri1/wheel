# API lane — handoff

Owner of `crates/wheel-api`, `crates/wheel-host`, `crates/wheeld`, `docker/Dockerfile.api|host`,
`docs/API.md`, `infra/`. Written at stand-down; everything below is on `origin/main`.

## STATE (verifiable on origin/main)

- **`1019bb7`** — `POST /v1/projects` now STARTS the sandbox (ARCHITECTURE M1: "create project →
  sandbox starts"). It did not, so the board answered `502 engine_unreachable` until someone called
  `/start`; that was CI's `WHEELD-engine-reachable`, and it was an API bug, not a `wheeld` bug. A
  sandbox that will not come up is `status: error` on a *created* project, never a failed create.
  Same commit: `wheeld` can bind engine sockets under a data dir of any depth (104-byte `SUN_LEN`
  on macOS; the run dir moves to a short private one when the natural place cannot hold a socket),
  and it obeys SIGTERM (the embedded engines install handlers of their own, and a *handled* signal
  stops terminating the process — so it kept serving after ctrl-c). `HostState.ready` is a
  `Readiness` with `serving_from_start` / `serving_after_reconcile`, replacing PM's flagged comment.
  QA's `test_wheeld_smoke.py`: **6/6**.
- **`02ce6a1`** (in that merge) — `infra/prune-probe-projects.sh` + `infra/tests/*.test.sh` (36
  assertions). **ADVERSARY APPROVED it for `--apply`** (`redteam/reviews/prune-probe-projects-review.md`),
  with three non-blocking notes, see NEXT.
- Earlier today: host binds `$PORT` and serves `/healthz` before reconcile (deploy
  `6d844531`, SUCCESS with the check ON); SQLite store for the API (`STORE=sqlite://…`); `wheeld`
  composition binary; F015 vault-key delivery; F017 prod auth interlock.

- **`8959cff`** — **CORS is no longer hand-listed.** `cors_layer` mirrors methods and headers from
  the preflight, with the origin allowlist as the only boundary. The list was a second copy of what
  the router serves and it drifted: the vault write is a `PUT`, `PUT` was missing, and the operator
  saw "Can't reach the API" — a preflight failure names neither the method nor the route, so the
  symptom pointed at the wrong thing entirely. A method we do not serve is now a `405` with a body.
  `crates/wheel-api/tests/cors.rs` is QA's **API-cors-covers-every-served-method**: it *parses the
  routes out of `src/lib.rs`* (a list there would be the same second copy), maps each verb to its
  methods (`any` → all seven), and preflights every (route, method) pair through the real router,
  failing if fewer than 15 routes parse. Negative control run and observed to fail: a static
  `[GET, POST]` gave "the router serves HEAD /healthz but the preflight allows GET,POST".
  Same commit: **ingress honesty** — a *bodiless* 404 from the engine becomes `501
  {"error":{"code":"ingress_unavailable",…}}`; a 404 the engine actually wrote (`no_such_endpoint`,
  or an endpoint script's own) passes through untouched. Do not widen that line to "any 404" or a
  script's own 404 becomes a lie. Web keys on the exact string `ingress_unavailable`; it is
  confirmed to them. `/p/{id}/{*rest}` also carries its own permissive CORS (`ACAO: *`, no
  credentials) so the board's "test this endpoint" button can read the reply — the URL is public by
  definition, so restricting who may read it protects nothing.

## IN FLIGHT

Nothing. The tree is clean and everything above is on `origin/main`.

## NEXT (priority order)

ADVERSARY's review is `redteam/reviews/prune-probe-projects-review.md`; its three non-blocking
notes are items 1–3 below.

1. **`example.com` in `PROBE_DOMAINS`** (ADVERSARY note 1) — the domain set is wide for a
   deny-by-default tool. Confirm signup rejects `@example.com`, or drop it from the list —
   otherwise a real-ish account prunes after 24h. Gate: a case in
   `infra/tests/prune-probe-projects.test.sh`.
2. **`delete_row` parameterisation** (note 2). `psql -v` instead of interpolating the uuid. Safe
   today because `is_uuid` admits only hex and dashes, and the SQLi payload is test-rejected.
3. **`set -e` mid-loop abort** (note 3). A failed `delete_row` aborts the run; the destroyed
   sandbox's row self-heals next run. Make the loop continue and report.
4. **M3 API surface, not started**: export/import routes, mail relay (`tool` kind `email`),
   operator-mode auth (`wheel login`, `--as`, `wheel token`).
5. **Per-node uid in the process backend** (ADVERSARY F007). Documented as a known gap at
   `docs/PROTOCOL.md:549`; the docker/process backends still run one uid per project.
6. **ADVERSARY finding 030 — `CARGO_HOME` shared across tenant uids on the process backend** (High
   there, Low on docker/M1). The fix is SDK's, but it lands in `docker/Dockerfile.host`, which is
   mine: the same image change that F003/F007 need. Whoever does per-node uids does this in the same
   pass — per-project `CARGO_HOME`/`RUSTUP_HOME` under the project data dir is already the M1.6
   contract (§6), so this is the contract not yet being true.

## TRAPS

- **Do not change Railway platform config without a way to verify it before it takes effect.** I
  took production down twice with `healthcheckPath`. The second time I was acting on a diagnosis I
  had already confirmed once and had stopped questioning. The real cause was neither path I tried:
  the host bound `0.0.0.0:7100` and ignored `$PORT`, so the checker probed a port nothing listened
  on and every path failed identically. Change one thing; prove reachability from inside the
  container first.
- **Railway silently ignores `null`** for `healthcheckPath` (returns success, keeps the old value).
  `""` works. `infra/railway/settings.json` must omit the key entirely or `apply-settings.sh`
  restores it.
- **A test can pass for the wrong reason.** My reaper test used `kill(pid, 0)`; an unwaited child is
  a zombie and still answers. Assert through `child.wait()`. Run a negative control before believing
  any new gate — I broke the CORS layer on purpose to watch that suite fail, and it is the only
  reason I trust it.
- **`cargo llvm-cov` is meaningless in this repo locally**: the shared `target-dir` mixes profraw
  across six worktrees (I once got 11%). Trust CI.
- **Never edit the main worktree, and check for another live session of your own role** before
  touching `wheel-wt/api` — another API session overwrote my `crates/wheeld/src/lib.rs` mid-edit.
  I combined both halves rather than clobbering; do that, do not reset.
- **`yoke msg` argv eats backticks and `$(…)`.** Always `--file`. My own messages arrived beheaded
  twice; both times the fix was already in the contract and I had not followed it.
- **Holding work uncommitted is how it disappears.** PM found 18 dirty files in my tree with no
  commit for 80 minutes and reasonably assumed my session had died. Uncommitted work is invisible to
  every other agent, blocks their rebases, and cannot be picked up if the session dies. Commit at
  every green point even if the story is unfinished; a WIP commit costs nothing and a lost tree
  costs an afternoon. Four separate losses today trace to this.
- **`wheeld`'s `composition` suite flakes under load.**
  `the_embedded_host_serves_and_runs_a_projects_engine` failed once while ~60 rustc processes were
  resident, and passed three times in a row afterwards, alone and with the package. I did not chase
  it. It is almost certainly a start timeout that is generous on an idle machine and not on a busy
  one; if you see it red in CI, that is the first place to look, not a real regression in the
  embedded backend.
- **Diagnose before you fix the thing you were handed.** `WHEELD-engine-reachable` was routed to me
  as "fix in wheeld". It was the API not starting the sandbox. Reproducing it took ten minutes and
  saved a day of looking in the wrong crate.

## CONTRACT (rules I think are wrong)

- **§0b "comments sparingly" is being applied to doc-comments that carry reasons.** The rule says a
  comment means the code does not describe itself — true of *what* comments, false of *why* ones. A
  named constructor cannot say "Railway's health-check window is shorter than reconciling fourteen
  tenants". Keep the rule for `what`, exempt `why` explicitly, or the next agent deletes the only
  record of a decision that cost two outages.
- **One host, one replica, no failover** (§5b) is accepted as v1 — but the two outages I caused were
  both total: no API, no boards, no agents. Before anyone dogfoods on this, the host needs either a
  second replica behind sticky routing or a documented "the whole product is down" runbook. I did
  not have one and improvised twice.
- **The A/B session partition by path did not work.** Both sessions of one role read the same
  mailbox and both believe they are the addressee. Partition by *worktree the session already
  holds*, decided by the session itself and announced, not by PM assigning paths to letters.
