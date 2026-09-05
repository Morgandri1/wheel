# 002 — Agents may run with bypassPermissions; sandbox is the only gate

- **Severity:** High (design-level)
- **Type:** DESIGN review. Contract does not state the `--permission-mode` agents launch with.
- **Owner:** SDK/Engine (agent supervisor / harness spawn)
- **Status:** OPEN (question + recommendation)
- **Boundary:** TB5, escalates via TB6/TB7.

## Claim
For agents to be useful headless (run scripts, edit worktree, call tools) they will almost certainly be spawned with `claude --permission-mode bypassPermissions` / `codex` equivalent — no human approval on tool use. That is a defensible product choice ONLY IF the sandbox around the child is a real, sole security boundary. Since agents are untrusted-by-assumption (prompt injection via ctx/messages/ingress), a bypass-mode agent = attacker with an unattended shell inside the sandbox. Every isolation claim (TB4 wire model, TB6 caps, TB7 uid) must therefore hold against an *actively hostile local shell*, not merely a careless one.

## Consequences to verify
- The wire/token model (TB4) is enforced by the ENGINE, not by the harness — an agent that ignores the CLI and writes syscalls directly must still be contained (fs perms, uid, network). If any wire check lives only in the CLI/MCP wrapper, bypass-mode defeats it.
- `/data/wheel.db`, sibling creds dirs, engine.sock, and secrets must be unreadable at the OS level, not just "the agent shouldn't".
- No `--dangerously-skip-permissions` path that also disables Wheel's own egress/SSRF controls.

## Recommendation (make binding)
1. Document the exact permission mode per harness in PROTOCOL.md, and that it is SAFE ONLY because of OS-level sandbox isolation — link the invariants in THREAT-MODEL §5.
2. Add a spawn-time assertion set (child uid != engine uid in process mode; child cannot open engine.sock; child cannot read wheel.db) that the engine self-tests on boot in a debug build.
3. QA + ADVERSARY co-own a "hostile shell" test: a script node that tries the full TB4/5/7 escape list and must fail every item.

## PoC plan
`redteam/pocs/002_hostile_shell.sh` run as a script node: attempt to read /data/wheel.db, connect engine.sock/:7000 with no token and with own token to a non-wired endpoint, read sibling env, spawn fork bomb (rlimit check). Assert all denied.

## Cross-ref (from api.md review): token-type discrimination at the engine
The API's authenticated proxy forwards any path — including `v1/cli/*` — to the engine with the HOST/engine
bearer. So invariant #2 has a second half: `/v1/cli/*` must require a per-NODE token AND reject the
host/engine bearer; control-plane routes must reject node tokens. The API cannot enforce this (it correctly
forwards); the engine must. Verify at engine impl. Owner: SDK/Engine.
