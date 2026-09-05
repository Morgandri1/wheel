# 007 — Per-node token/creds isolation collapses within a project (same uid)

- **Severity:** High (intra-tenant; defeats a stated security control) — needs a PM residual-risk ruling
- **Owner:** SDK/Engine (+ API for the uid model) + PM (accept/mitigate)
- **Status:** OPEN — surfaced in the sdk.md plan review (pre-code).
- **Boundary:** TB4 (engine ↔ child) + TB7 (tenant isolation).

## Claim
§2 assigns **one unix uid per PROJECT** (process backend), so every agent/script/MCP child in a
project runs as the **same uid**. The per-node capability model (distinct `WHEEL_TOKEN` per node, wire
matrix, §3e grant attenuation) assumes a node cannot use another node's authority. Same-uid defeats
that: a prompt-injected agent A (bypassPermissions RCE, finding 002) can read sibling agent B's
`WHEEL_TOKEN` via `/proc/<B-pid>/environ`, and B's OAuth creds via B's `CLAUDE_CONFIG_DIR`/`CODEX_HOME`
under `/data/projects/<id>/...` — both readable at the same uid. A then acts with B's full wire set.

`hidepid` does NOT help: it blocks *cross-uid* `/proc`, not same-uid. So within a project, per-node
wire scoping and §3e attenuation (e.g. A has vault `read`, B has `write` → A steals B's token → A
writes) are **not enforced at runtime**.

## Impact
Any single injected agent escalates to the union of ALL sibling nodes' capabilities in that project.
Confined to one user's own project (no cross-tenant), but it nullifies per-node wires, grants, and
attenuation — all stated controls the UI/§3e imply are real.

## Options (PM to rule)
1. **Accept + document:** "the PROJECT, not the node, is the runtime isolation boundary; a compromised
   agent compromises all agents in the same project." Then §3e attenuation is a UX/audit control, not a
   security boundary — must be said plainly in ARCHITECTURE.md and the §3e docs.
2. **Per-node uid** (sub-uid range per project): real intra-project isolation; costs uid management.
3. **Token not in env / short-lived + bound:** reduces token theft via /proc/environ but NOT creds-dir
   theft; partial only.

## Proposed action
SDK+PM decide 1 vs 2. If (1), I'll add it to THREAT-MODEL residual risks and downgrade any finding that
assumed per-node runtime isolation. PoC (once process backend exists): agent A reads B's token from
/proc and performs a B-only wire action.

## RULING — PM, ACCEPTED (per-node uid; contract §2/§3)
Chose option 2 (per-NODE uid), not accept-and-document. Design: project uid RANGE; engine runs as the
base uid with **ambient CAP_SETUID/CAP_SETGID only**; each child gets its OWN uid; creds dirs 0700;
shared workspaces via setgid; **`WHEEL_TOKEN` delivered via a 0600 file, not env** (kills the
/proc/<pid>/environ theft vector). Lands M2 (docker) / M3 (process). Until M2, PROTOCOL.md states the
gap. Status → ACCEPTED, awaiting M2 impl to verify (PoC: cross-node /proc + token-file read must be
EACCES). Owner: SDK/Engine (setuid per child, token file) + API (host grants the two ambient caps).
