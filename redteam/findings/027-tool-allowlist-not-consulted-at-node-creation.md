# 027 — WHEEL_TOOL_ALLOW_HOST is consulted at call time but NOT at tool-node creation → allowlist unusable via the API (blocks the send() allowed-path tests)

- **Severity:** Low (testability / feature-completeness gap; fails CLOSED — nothing is *widened*). Owner:
  **SDK/Engine** (tool-node create validation vs `execute::Allowlist`).
- **Status:** CONFIRMED LIVE — fresh image from HEAD 2a50695, `WHEEL_TOOL_ALLOW_HOST=127.0.0.1:18080,127.0.0.1:18081`.
  PoC: `redteam/pocs/tool-exec/run_tool_allowed.sh`.
- **Boundary:** TB7 (tool nodes). Not a vuln — but it blocks the very allowed-path verification the allowlist
  was built to enable, so it matters for finishing 022/026/send() e2e coverage.

## What
The allowlist is consulted only in `execute::resolve_for` at CALL time (`tool_routes.rs:376` builds
`Allowlist{targets:&cfg.tool_allow_hosts}` and passes it to `send`). **Tool-node CREATION validates
`base_url` against `host_is_denied` and does NOT consult the allowlist.** So creating a tool whose `base_url`
is the allowlisted target is refused:
```
POST /v1/nodes  base_url=http://127.0.0.1:18080  (with WHEEL_TOOL_ALLOW_HOST=127.0.0.1:18080)
-> 400 "tool base_url host \"127.0.0.1\" is not reachable: private, loopback or internal addresses are denied"
```
The allowlist is keyed on the LITERAL host:port (`allow.permits(host, port)`), and the only literal that
matches is a loopback/private one — which creation rejects unconditionally. Catch-22: **no tool that the
allowlist would permit at call time can ever be created through the API.** A hostname that *resolves* to the
allowlisted IP does not help either — `permits` compares the literal host string (`"myecho" != "127.0.0.1"`),
so it is denied at call time.

## Why it matters
`config.rs` states the allowlist "exists to let tests and red-team probes reach a local server." As shipped it
cannot: the node API refuses to create the tool. Concretely this **blocks the allowed-path send() live tests**
SDK asked for — header-CRLF-at-send (the top priority), redirect-no-replay, per-hop re-validation, the 5 MiB
cap and the 30 s timeout — because none can be exercised through `/v1/tools/:id/call` without a creatable
allowlisted tool. Those paths currently have SDK's unit coverage only; there is no end-to-end route to verify
them, which is the exact gap ("send() had no coverage") that this allowlist was meant to close for red-team.

## Repro / evidence (all live, this run)
- Boot WARN correctly names the targets ✓. prod-boot-refusal ✓ (control: prod w/o allowlist boots exit 0;
  prod w/ allowlist refuses exit 2 — "WHEEL_TOOL_ALLOW_HOST is set (…) but WHEEL_ENV=prod: …must never be set
  in production").
- Create with `base_url=http://127.0.0.1:18080` → 400 (above), despite the allowlist naming that exact target.
- `build_request` witness (dry_run, public base_url): a CRLF header value is carried VERBATIM into
  `Prepared.headers`: `-H 'X-Try: a\r\nX-Injected: pwned'`. So `build_request` does not sanitize CRLF; the sole
  defense is reqwest's typed `HeaderValue` at send() — which I could not reach via the route because of this
  gap. Header-CRLF-at-send therefore remains reasoned-not-demonstrated, blocked here.

## Fix (proposed)
Make tool-node create/re-import base_url validation consult the same allowlist `send` uses: permit a
`base_url` whose host:port is in `WHEEL_TOOL_ALLOW_HOST` (exact match), otherwise apply `host_is_denied` as
today. One shared check for create and call. Then the allowlist does what its own comment promises, and I can
finish the header-CRLF / redirect / cap / timeout live tests through the route. (Alternatively, expose a
test-only path to construct a `Prepared` against an allowlisted target — but consulting the allowlist at
create is the consistent fix.)

## Not affected / verified good this run
026 (6to4/NAT64/Teredo) VERIFIED FIXED live (see finding 026). Prod-boot-refusal + boot WARN verified. The
allowlist does not WIDEN anything reachable (the gap is over-restriction, not under-restriction).
