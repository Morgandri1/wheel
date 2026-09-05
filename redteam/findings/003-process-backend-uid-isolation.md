# 003 — Process backend: uid-per-project is the ONLY tenant isolation

- **Severity:** Critical (design-level; realizes at M3 when backend lands)
- **Type:** DESIGN review of §2 process backend + §5b + §5b residual-risk note.
- **Owner:** API (wheel-host) + SDK/Engine (drops-to-uid contract)
- **Status:** LIVE-VERIFIED for the engine layer (2026-09-05, image `wheel-engine:dev` built from main @ d65166e, `WHEEL_ROLE=host` + `SANDBOX_BACKEND=process`). Invariants 2,3,4,6,7 hold live (10/10). Invariant 1 (agent/script/MCP *children*, not just the engine) and invariant 5 (Railway egress) remain to verify — see "Still open" below. PoC: `redteam/pocs/child-isolation/run_process_backend_campaign.sh` + `t_process_backend_isolation.py`.
- **Boundary:** TB7 tenant ↔ tenant.

## LIVE VERIFICATION (2026-09-05) — engine layer, two real tenants A/B on one kernel

Boot host in process mode, PUT+start two projects through the real host API (A→uid 20000, B→uid 20064),
then probe AS uid A against B. **10/10 PASS, exit 0:**

| # | check | result |
|---|-------|--------|
| 2 | `/data/projects/<B>` read cross-uid | DENIED (drwx------ 20064) |
| 3 | `/run/wheel/<B>/engine.sock` connect cross-uid | DENIED (0700 dir) |
| 4 | read engine B's real `/proc/<pidB>/environ` (holds B's `WHEEL_ENGINE_SECRET`+`WHEEL_VAULT_KEY`) | DENIED |
| 5 | read host PID 1 `/proc/1/environ` (holds `WHEEL_HOST_SECRET`) | DENIED |
| 6 | `WHEEL_HOST_SECRET` in engine child's own env | ABSENT — `env_clear()` in process.rs; **and** the setuid drop clears `dumpable`, so the child's environ is EACCES even to container-root without CAP_SYS_PTRACE (bonus hardening) |
| 7b | write into `<B>`'s run/data dir cross-uid | DENIED |
| 7 | engine `RLIMIT_NPROC` (real child `/proc/<pid>/limits`) | 512 (finite fork-bomb cap, not the container default) |
| 8 | `NoNewPrivs` | 1 |
| 9 | supplementary groups (`setgroups([])`) | cleared (`Groups: `) |
| 10 | engine real Uid/Gid | 20000/20000 across all four fields (full drop, not gid 0) |

**Invariant-4 hidepid flag DOWNGRADED:** kernel uid perms on `/proc/<pid>/environ` + the `dumpable=0`
that setuid sets already deny cross-tenant secret reads *without* `hidepid`. `hidepid` would only
additionally hide pid *enumeration*; it is not load-bearing for secret protection. (Confirmed live.)

### PROBE-VEHICLE TRAP (documented so it is never re-filed as a false Critical)
A first pass using `docker exec -u <uid> … cat /proc/self/environ` / `ulimit -u` reported HOST_SECRET
"leaking" and "no rlimit". **False positives:** `docker exec` spawns a *new* process that inherits the
CONTAINER PID-1 env (which legitimately holds `WHEEL_HOST_SECRET`, passed at `docker run`) and the
container-default rlimits — NOT the engine child's `env_clear()`'d env or its `pre_exec` setrlimit.
Every env/rlimit/drop assertion MUST read the **real engine OS pid** (`/proc/<ospid>/…`); cross-tenant
reach is a read of the *sibling's* real pid, never a self-read. The probe now enforces this and carries
the note inline.

## Claim
In prod there is no container-per-tenant: all projects share ONE kernel and ONE container, isolated only by a per-project unix uid, dir mode 0700, and a uid-owned engine.sock. This is a weak boundary against an actively hostile agent (see 002). Any gap = cross-tenant compromise of A5/A6, and reaching *.railway.internal exposes A3/A9.

## Must-hold invariants (each becomes a test the day the backend exists)
1. EVERY child (agent, script, MCP server) runs as the project uid — not just the engine. Verify via /proc/<pid>/status Uid line for each spawned pid.
2. `/data/projects/<other>` unreadable/unwritable cross-uid (0700 + correct owner; no world/group bits; parent dir not traversable to enumerate ids).
3. `/run/wheel/<other>/engine.sock` un-connectable cross-uid (socket file perms + SO_PEERCRED check in engine — engine should also verify peer uid, defense in depth).
4. `/proc/<pid>/environ` of another tenant's pids unreadable → requires `hidepid=2` mount or equivalent; WITHOUT it, A2/A4/A5/A8 leak trivially. **Flag: contract does not mention hidepid.**
5. No egress to *.railway.internal / host :7100 / Postgres from inside a sandbox — needs a network namespace or egress firewall per project; contract's §5b residual note acknowledges the risk but states no control. **Flag: define the control.**
6. rlimits (nproc, nofile, cpu, as) + a pids cgroup so one tenant cannot fork-bomb the shared machine.
7. setuid drop correctness in host: setgroups([]) to clear supplementary groups, set gid before uid, set no_new_privs, verify with a re-exec probe.

## Open questions for PM/API
- Is there a per-project network namespace, or only uid? (Determines whether TB7 net attacks are even mitigated.)
- Is `hidepid` planned for the /proc mount?

## Still open (not yet verified live)
- **Invariant 1 for agent/script/MCP children.** I verified the *engine* drops to the project uid; I did
  NOT yet verify that every spawned agent/script/MCP child ALSO runs as a project uid (§2 says base+1+n
  per node). That needs an authenticated agent or a script node running in-sandbox; probe staged.
- **SO_PEERCRED (invariant 3 defense-in-depth).** Socket is uid-owned + 0600 in a 0700 dir (sufficient
  on its own). Whether the engine ALSO checks peer uid via SO_PEERCRED — SDK was adding — not yet asserted.
- **Invariant 5 (egress to *.railway.internal / host :7100 / Postgres).** Railway-only; must be run on
  the deployed host against our own infra, never from a laptop. The probe leaves it commented for that reason.

## PoC plan
`redteam/pocs/child-isolation/run_process_backend_campaign.sh` (orchestrator) + `t_process_backend_isolation.py`
(checks 2–10). Runs today against the local combined image; the same probe extends to agent/script children
once the harness path lands, and to egress on the Railway host.
