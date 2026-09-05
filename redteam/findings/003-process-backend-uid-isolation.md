# 003 — Process backend: one kernel + one container for all tenants

- **Severity:** Critical (once process backend exists) — highest system impact
- **Owner:** API (wheel-host) + SDK/Engine (spawn/setuid contract)
- **Status:** OPEN — design review (pre-code). Test matrix pre-written; runs the day the backend lands.
- **Boundary:** TB6.

## Claim
In prod (§5b) every project's engine, agents, scripts and MCP servers run as `process` sandboxes on
ONE Railway machine, in ONE container (host runs as root, setuids per project), sharing one kernel and
one private network. Isolation reduces to: (a) per-project uid correctness, (b) filesystem perms, (c)
egress filtering. There is NO namespace/container boundary between tenants. A single gap in any of
these is a full cross-tenant breach — and every agent is a bypassPermissions RCE (see 002).

## Attack surface / required invariants (each becomes a test)
1. **uid enforcement for EVERY child** — agent, script, MCP `command`. If any child path forgets the
   setuid drop, it runs as root or another uid. Test: from each child type, `id` must equal the project
   uid; `/data/projects/<other>` (0700) and `/run/wheel/<other>/engine.sock` must be EACCES.
2. **/proc leakage** — `/proc/<pid>/environ` of another tenant's engine/agent exposes A3/A4/A5/A6.
   Mitigation: `hidepid=2` mount (or equivalent) so a uid can't see others' /proc. Test: cross-uid
   `cat /proc/<other-pid>/environ` must fail.
3. **Egress** — agents/scripts reaching `*.railway.internal` = Postgres (A10) and host `:7100` (A2).
   Must be blocked by egress policy (the SSRF deny-list covers the tool executor, but raw agent shell
   `curl http://wheel-host.railway.internal:7100` is NOT behind the tool executor). This needs a
   network-level egress control, not just app-level deny-lists. **Flag: contract's SSRF deny-list
   protects the tool node but not arbitrary agent/script outbound — that is a gap.**
4. **Host secret** (A2) must never be in any sandbox env or reachable file.
5. **rlimits / fork bomb** — a project must not starve the shared machine (nproc, cpu, mem, fsize,
   nofile). Test: fork bomb + big-file write from one project; others stay responsive.
6. **setuid-drop correctness** — supplementary groups cleared (`setgroups`), `no_new_privs` set, real
   AND effective + saved uid all dropped, cwd not left in another project's dir.

## Proposed action
Pre-write pocs/process-backend/ matrix now (skipped until backend exists). Raise item #3 (arbitrary
agent egress to railway.internal) as a distinct HIGH now — it is a design gap, not just an impl bug.
