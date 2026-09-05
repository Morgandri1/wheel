# 004 — Tool-node SSRF: public-IP check is bypassable by construction

- **Severity:** High (design-level)
- **Type:** DESIGN review of §3d rule (4) SSRF policy + executor.
- **Owner:** SDK/Engine (tool executor)
- **Status:** OPEN
- **Boundary:** TB8 tool → internet; pivots to TB7/TB3.

## Claim
§3d(4) says base_url + every redirect must resolve to a public IP (deny loopback/RFC1918/link-local/*.railway.internal/host). A naive implementation (parse host, one getaddrinfo, string-match) is bypassable and would let a prompt-injected agent reach Postgres, the host, or cloud metadata.

## Bypass classes to defend
1. **DNS rebinding / TOCTOU:** resolve-then-connect uses two lookups; attacker returns public IP on check, private on connect. → Must resolve ONCE and connect to that exact IP (pin), for base_url AND every redirect hop.
2. **Encoding:** IPv6 (`[::1]`, `::ffff:127.0.0.1`, `[::]`), octal `0177.0.0.1`, decimal `2130706433`, hex `0x7f.1`, trailing-dot `metadata.google.internal.`, `0.0.0.0`, `127.1`.
3. **Redirect chain:** public base_url 302→ `http://169.254.169.254/` (MOCKED) or `http://wheel-p-<other>:7000`. §3d says follow ≤3 redirects — each hop must be re-validated with the same pinned-resolve rule.
4. **Name tricks:** `*.railway.internal`, `*.internal`, `localhost.`, unicode/IDN homoglyph hosts, userinfo `http://public@127.0.0.1/`.
5. **Protocol:** non-http scheme, `file://`, `gopher://` if the client follows.
6. **CRLF injection** via agent-filled header/query fill → request smuggling / extra headers.
7. **Fill override:** agent supplies a duplicate/case-variant key or JSON-pointer collision to override a `static`/`vault` fill (must be rejected, §3d rule 1-2).
8. **Import DoS:** YAML bomb / $ref cycle / multi-MB spec starving the parser (single-threaded engine).

## Required invariants (proposed)
- Resolve host to IPs, filter ALL of them against the deny set (v4+v6, all encodings normalized), connect to a pinned allowed IP, disable further DNS; re-run on every redirect. Reject non-http(s), userinfo, and any host that resolves to zero public IPs.
- Strip/reject CR/LF/NUL in any agent-filled header/param value.
- Enforce fill precedence server-side by rebuilding the request from the schema, not by merging agent JSON over a template.
- Import parser: byte cap, depth cap, $ref cycle detection, timeout.

## PoC plan
`redteam/pocs/004_ssrf_suite.py` + a mock metadata server on 169.254.169.254 (via loopback alias / mock host) and a rebinding DNS stub. Runs when executor exists.
