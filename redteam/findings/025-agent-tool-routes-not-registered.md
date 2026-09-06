# 025 — Agent-side tool routes exist but are NOT registered → agents cannot call tools

- **Severity:** Low as security (fail-CLOSED: no agent tool access), but a **functional break** of the whole
  tool feature for agents, and it is believed live. Owner: **SDK/Engine** (`crates/wheel-engine/src/api/mod.rs`).
- **Status:** CONFIRMED live (image 23:48Z, routes @ 6c371c7). `POST /v1/cli/tool` → empty-body **404**
  (axum unmatched route). PoC: `redteam/pocs/tool-exec/t_tool_http.py` ("(wiring) agent /v1/cli/tool is
  reachable" → FAIL). Boundary TB6 (child↔CLI).

## What
`cli_routes::tool_ls` (`GET /v1/cli/tool`) and `cli_routes::tool_call` (`POST /v1/cli/tool`) are implemented
(cli_routes.rs:620/649) but the `cli` Router (`api/mod.rs`, the `let cli = Router::new()...` block) registers
only whoami/connections/ls/list/read/secret/secret-keys/write/msg/inbox/rm/query — **no `/tool` route**. So
the agent-facing endpoints (and `wheel tool ls|call`) 404. Agents cannot enumerate or call tool nodes; the
built-in MCP `<tool>__<op>` exposure (§3c#1), which feeds off the same agent path, is affected too.

The operator route `/v1/tools/:id/call` IS registered and works (it shares `run_operation`), so the executor
itself is exercised and correct (see 022 verified-fixed) — only the agent's door is missing.

## Why it matters to red-team
1. It's the kind of gap a "route landed" status hides: the handlers compile and the operator path works, so
   tests pass, but the agent — the actual consumer — gets 404. (It also silently invalidated my first e2e
   run's executor assertions with false passes; I caught it via an explicit reachability probe.)
2. Fail-closed today, but when it's wired, the agent path must get the SAME `resolve_vault_fills` wire-gating,
   fill refusal, and SSRF/mask guarantees the operator path has. Re-verify 022/fill-precedence/SSRF on the
   agent path once registered.

## Fix
Register the routes on the `cli` Router:
```rust
.route("/tool", get(cli_routes::tool_ls).post(cli_routes::tool_call))
```
and add a smoke test that a node-token GET/POST `/v1/cli/tool` for a wired tool returns 200 (not 404).

## Bonus (Informational, not a finding) — config-time SSRF string-check misses numeric IPs
`validate_tool` rejects a denied `base_url` at node creation via `host_is_denied` (a literal/suffix STRING
check): `127.0.0.1`, `169.254.169.254`, `10.0.0.1`, `*.railway.internal`, `file:` are refused at create.
But `http://2130706433/` (decimal), `http://0177.0.0.1/` (octal), `http://0x7f000001/` (hex) PASS create
(201) — the host string isn't a literal private form. **Runtime `resolve_and_check` catches all three** at
call time ("127.0.0.1 is not a reachable destination"), verified live, so there is no SSRF — the config-time
check is just an early UX filter. Optional: normalise/parse the host to an IP in `validate_tool` too, so a
decimal/octal/hex loopback is rejected at create with a clear error rather than only failing at first call.
