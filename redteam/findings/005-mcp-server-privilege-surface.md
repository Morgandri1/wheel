# 005 — Built-in MCP server as a new privilege surface

- **Severity:** High
- **Owner:** SDK/Engine
- **Status:** OPEN — design review (pre-code).
- **Boundary:** TB5 (child ↔ CLI/MCP) + TB4 (engine enforcement).

## Claim
§3c#1 attaches a built-in MCP server (`wheel mcp-serve`, stdio) to EVERY agent, forwarding to the
engine with the node token, exposing tools: msg, read, write, rm, ls, query, secret_get, run,
ctx_clear, inbox, whoami, connections. §3d also generates `<tool>__<op>` MCP tools. This is a second
capability entry point beside the CLI — it must enforce EXACTLY the same wire gating and NOTHING more.
Under bypassPermissions the agent also controls its own harness MCP config file, so it can try to
reconfigure or add MCP servers.

## Attack surface / invariants
1. **Same gate as CLI:** every MCP tool call must re-check the caller's wires via the node token,
   returning the equivalent of exit-3 denial. No MCP tool may bypass a check the CLI enforces
   (esp. `query` read-only-single-table, `secret_get` vault-wire, `write` implies-read).
2. **Tool-arg injection:** `<tool>__<op>` schemas must expose ONLY agent-mode fields (§3d rule 1);
   the MCP layer must reject extra/non-agent fields exactly like the CLI, else static/vault fills are
   overridable via the MCP path even if the CLI path is safe. Test both paths identically.
3. **No stdin write via MCP (cross-ref 007/§3c#12):** `msg`/`ctx_clear` over MCP must go through the
   same durable queue + single-writer delivery loop; the MCP server must NOT write to any child stdin.
4. **inbox as info-leak:** `inbox` must return ONLY the calling node's received messages — never
   another node's. A node must not read a sibling's inbox by id guessing (ids are envelope UUIDs;
   ensure authz is by to_node = caller, not by id lookup alone).
5. **Config tampering:** since the agent can edit its harness MCP config under bypassPermissions,
   the engine must (a) treat the child as untrusted regardless of its MCP config, (b) not rely on the
   child only having the built-in server — enforcement is server-side on the token, so an added rogue
   MCP server gains nothing it couldn't already do via shell. Confirm no ambient authority is granted
   by mere presence in the config.

## Proposed action
To SDK: one shared authz function behind BOTH the `/v1/cli/*` and MCP forwarders (so they cannot
drift); table-driven test asserting CLI and MCP give identical allow/deny + identical field rejection
for the full 9×9×3 matrix (dovetails QA's 243-cell matrix). PoC once bootable.
