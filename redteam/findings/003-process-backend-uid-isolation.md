# 003 — Process backend: uid-per-project is the ONLY tenant isolation

- **Severity:** Critical (design-level; realizes at M3 when backend lands)
- **Type:** DESIGN review of §2 process backend + §5b + §5b residual-risk note.
- **Owner:** API (wheel-host) + SDK/Engine (drops-to-uid contract)
- **Status:** OPEN (pre-write test matrix)
- **Boundary:** TB7 tenant ↔ tenant.

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

## PoC plan
`redteam/pocs/003_cross_tenant_matrix.sh` — create projects A,B; from A's script node attempt each of 1-7 against B; assert denials. Pre-written now; runs at M3.
