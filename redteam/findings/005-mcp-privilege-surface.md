# 005 — MCP node privilege surface: arbitrary exec + attach as engine uid

- **Severity:** High (design-level)
- **Type:** DESIGN review of §3 mcp config + §3 wire matrix (agent → mcp) + §3c#1 built-in MCP.
- **Owner:** SDK/Engine
- **Status:** OPEN
- **Boundary:** TB5 child ↔ MCP; escalates TB6/TB7.

## Claim
An `mcp` node with `transport:"stdio", command, args, env` causes the engine to exec an arbitrary binary and attach it to an agent's harness. Two distinct risks:
1. **Arbitrary code exec as the engine/project uid:** `command` = any binary/path with any args/env. Who authors it? If an agent (or prompt-injected operator flow) can create/edit an mcp node's `command`, that's RCE-by-design inside the sandbox — acceptable ONLY if the sandbox uid boundary (002/003) is airtight AND mcp children run as the SAME confined uid, never the engine's more-privileged context. Verify the mcp child inherits the project uid, rlimits, no_new_privs, and cannot open engine.sock/wheel.db.
2. **`http` transport = SSRF:** `mcp.url` is another egress path (like tool base_url, 004) — must go through the SAME public-IP/pinned-resolve deny policy. Contract's §3d SSRF policy is scoped to tool nodes; **flag: extend it explicitly to `mcp.url`.**
3. **Env leakage:** `mcp.env` could be set to exfiltrate (`{FOO: $SECRET}`) — confirm env values are literal, not expanded against engine env, and that vault refs are the only secret path.
4. **Built-in MCP (§3c#1) trust:** the engine attaches `wheel mcp-serve` forwarding to the engine with the node token. Confirm the built-in server binds the node token from the child's OWN credentials (not a shared secret) and that a malicious external MCP server attached to the same agent cannot observe/replay the built-in server's token or the agent's stdin.

## Required invariants (proposed)
- mcp `command` children run as the project uid with the same sandbox confinement as scripts; document it.
- `mcp.url` (http transport) subject to the §3d(4) SSRF policy verbatim — add to contract.
- Who may create/edit mcp nodes: restrict to the operator via API; agents get agent→mcp READ (attach) only, never node config write (already implied by wire matrix — confirm engine enforces that node-config mutation is API-only, never a CLI/MCP capability).
- Built-in MCP token is per-node and never exposed to sibling MCP servers or in env visible to other children.

## PoC plan
`redteam/pocs/005_mcp_exec.sh` — mcp node with command=/bin/sh reading secrets; assert it runs as project uid, cannot reach engine.sock/wheel.db, and mcp.url=169.254.169.254 (MOCKED) is denied.
