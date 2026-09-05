# 004 — Tool-node SSRF surface + arbitrary-agent egress gap

- **Severity:** High
- **Owner:** SDK/Engine (tool executor) + API/host (network egress)
- **Status:** OPEN — design review (pre-code).
- **Boundary:** TB7 (+ TB6 for raw egress).

## Claim
§3d defines a solid SSRF deny-list for the TOOL executor (loopback/RFC1918/link-local/*.internal/
*.railway.internal, incl. redirect+DNS, ≤3 redirects, ≤5 MiB, 30s). Two gaps:
1. **DNS rebinding / encoding:** deny-list must resolve-then-pin the IP actually connected to (not
   just validate the hostname), and re-validate every redirect target's resolved IP. Must reject
   IPv6-mapped (`::ffff:127.0.0.1`), octal/decimal/0x/short IP forms, and `[::1]`. If validation is on
   the hostname string only, rebinding (TTL=0, A record flips after check) bypasses it.
2. **Raw egress is NOT behind the executor:** an agent under bypassPermissions can `curl`/`python
   -c` directly to `169.254.169.254`, `wheel-host.railway.internal:7100`, `wheel-p-<other>:7000`, or
   Postgres. The §3d deny-list does nothing for this. Cross-tenant/metadata protection needs a
   NETWORK egress policy applied to the whole sandbox, not just the tool executor.

## Also (tool executor specifics)
- CRLF/header injection via agent-filled header/param values → request splitting.
- Agent overriding `static`/`vault` fills via duplicate JSON keys, case-variant keys, JSON-pointer
  collisions — precedence must be enforced by the engine merge, extras rejected (400 + denial event).
- Vault value echo: resolved `vault` values must never appear in the returned body passthrough,
  event log `{tool,op,status,duration_ms,bytes}`, or `--curl` output (must be masked).
- Malicious spec DoS: YAML bomb / `$ref` loop / huge body against the single engine parser → bounded
  size, depth, ref-expansion, and time.

## Proposed action
- To SDK: resolve-and-pin IP validation + per-redirect re-validation + encoding normalization; merge
  precedence with extra-field rejection; vault masking in all three output paths; parser bounds.
- To API/host (via PM): a real network egress deny for sandboxes (metadata + RFC1918 + *.internal +
  sibling engines) — the app-level deny-list is necessary but not sufficient. Cross-ref 003#3.
- PoC (once bootable): mock 169.254.169.254 + a rebinding DNS in pocs/ssrf/.
