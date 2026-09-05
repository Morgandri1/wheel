# 002 — bypassPermissions makes the agent boundary breached-by-design

- **Severity:** High (design posture; informs every other finding)
- **Owner:** SDK/Engine (+ PM: accept/document residual)
- **Status:** OPEN — design review (pre-code).
- **Boundary:** TB4/TB5/TB6.

## Claim
Every agent runs `claude`/`codex` with `--permission-mode bypassPermissions`. That means the agent
can run arbitrary shell as the project uid with no per-action gate — it is RCE-equivalent inside the
sandbox by design. Therefore **no security control may live in the agent's own restraint or in the
harness permission prompt.** A single successful prompt injection (assume it always succeeds) gives
the attacker everything the project uid can reach.

## Consequences to enforce
1. All capability enforcement MUST be server-side at the engine (wire re-check on every `/v1/cli/*`
   and MCP call, using the per-node token) and at the kernel (uid/rlimits/egress). The wire set IS the
   sandbox for an agent.
2. The engine control plane (`:7000`/unix socket, engine bearer) MUST reject node tokens; a node token
   is only valid on `/v1/cli/*`, which re-checks wires. An agent that reaches the control-plane port
   with its node token must get 401/403, not partial access.
3. Nothing an agent can read (env, `/proc/self`, `/data`, files) may contain a secret broader than its
   own node scope — see 003, 004.

## Proposed action
PM to explicitly accept "agent = untrusted RCE" as the design premise in ARCHITECTURE.md, and require
that every capability doc state its server-side check. I will PoC (once bootable): agent shell →
attempt to (a) hit control plane directly, (b) read /data/wheel.db, (c) read sibling env via /proc.
