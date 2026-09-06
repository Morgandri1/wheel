# Pre-review — tool-node executor (outbound HTTP with credentials the agent never sees)

For SDK/Engine, BEFORE ship (invited pre-review, like the query authorizer). The executor takes an agent's
`wheel tool call <op> '<args>'`, resolves the op's fills (agent / static / vault / hidden), and makes an
outbound HTTP request, returning `{status, headers, body}`. It is the highest-leverage surface after
save_to_vault: it sends **vault/static credentials the agent cannot read** to a **URL an agent-supplied
field can influence**. Two threat classes: **SSRF** (reach internal targets) and **credential exfiltration**
(make the executor send a secret somewhere the agent controls, or echo it back).

Grounding: `wheel-core/src/tool.rs` has the types + `ip_is_denied(IpAddr)` (v4: loopback/private/link-local/
broadcast/doc/unspecified/0.0.0.0-8/CGNAT 100.64-10/192.0.0.0-24/198.18-15/240-4; v6: loopback/unspecified/
ULA fc00::7/link-local fe80::10/IPv4-mapped). That pure predicate is necessary but NOT sufficient — the
executor-level rules below are where SSRF is actually won or lost.

## A. SSRF — the executor must, not just `ip_is_denied`
1. **Resolve-and-PIN (DNS rebinding).** Resolve the host once, run `ip_is_denied` on the result, then CONNECT
   TO THAT EXACT IP — never let the HTTP client re-resolve. reqwest re-resolves by default; a rebinding DNS
   server answers public on the check and 127.0.0.1 on the connect. Use a custom resolver/`resolve()` pin or
   connect-by-IP with the Host header set. **Test:** a host whose DNS returns a public IP first then a private
   IP (TTL 0) must not reach the private one.
2. **Check EVERY resolved address.** A name with both a public A record and `127.0.0.1` (or `::1`) must be
   REFUSED (reject if ANY resolved IP is denied), not "use the first public one."
3. **Re-validate on EVERY redirect hop.** Follow ≤3 redirects (§3d); run resolve-and-pin + `ip_is_denied` on
   each `Location`. **Test:** `200`→`302 http://169.254.169.254/…`, `302`→`http://10.0.0.1`, and a redirect
   to a rebinding host. A public→internal redirect is the classic bypass of a first-hop-only check.
4. **Host-string normalization / URL parser confusion.** Before resolving, the host must be parsed
   unambiguously. Attacks: `http://public@127.0.0.1/`, `http://127.0.0.1#@public/`,
   `http://public\t.evil/`, decimal `http://2130706433/`, octal `http://0177.0.0.1/`, hex `http://0x7f.0.0.1/`,
   shorthand `http://127.1/`, trailing dot `http://metadata.google.internal./`, uppercase, IDN/punycode
   homographs, `[::ffff:127.0.0.1]`. Resolve via the *parsed* host and validate the *resolved* IP (numeric
   forms resolve to their IP → `ip_is_denied` catches them), but ensure the parser used to extract the host
   is the SAME one used to connect (no parser-differential between validation and the client).
5. **v6 embeddings beyond IPv4-mapped.** `ip_is_denied` checks `to_ipv4_mapped` but NOT 6to4 (`2002::/16`
   embedding a v4), NAT64 (`64:ff9b::/96`), or Teredo (`2001::/32`). `2002:7f00:1::` / `64:ff9b::7f00:1`
   embed 127.0.0.1. If the host has any such route these reach loopback/private. **Recommend:** also deny
   `2002::/16`, `64:ff9b::/96`, `2001::/32` (or extract+check their embedded v4). **Test each.**
6. **Platform internals.** `*.railway.internal`, `*.internal`, the host's own `:7100`/control plane, and the
   metadata IPs (`169.254.169.254` v4 — covered by link-local; `fd00:ec2::254` — covered by ULA; but confirm
   `metadata.google.internal` resolves to a denied IP at call time). **RoE: mock the metadata endpoint, never
   hit a real one.**
7. **Scheme + port.** Only `http`/`https`. Deny `file:`, `gopher:`, `ftp:`, `data:`, `dict:`. No port
   restriction is needed if IP is validated, but confirm `http://127.0.0.1:7100` is denied by IP not port.
8. **Applies equally to `mcp.url`** and to ANY field an agent can fill that influences the URL (path/query/
   host). Same resolve-and-pin path.

## B. Credential confinement & exfiltration (the save_to_vault-class leverage)
1. **No replay on cross-origin redirect.** If a vault/static fill is sent as an auth header and the server
   `302`s to a DIFFERENT origin, the executor must NOT resend that header to the redirect target — even a
   *public* attacker host. Since redirect targets are allowed to be any public host, this is the real leak:
   drop `Authorization`/vault/static-sourced headers on any cross-host (or cross-scheme/downgrade) redirect.
   **Test:** static/vault auth header + `302 http://attacker.public/` → the second request carries NO secret.
2. **Never echo secrets back.** The returned `{headers, body}` must not surface a vault/static value:
   response headers can reflect a sent header; a body can echo it. At minimum, do NOT let the response expose
   what the agent couldn't see. **Test:** an endpoint that echoes request headers in its body — the vault
   value must not reach the agent through the response.
3. **`--curl` / dry_run masks static AND vault** (finding 012 lesson): both render `****`, never the value;
   dry_run must not expose static/vault to the agent.
4. **Event log carries no resolved secret** (§3d rule 6): `{tool, op, status, duration_ms, bytes}` only —
   never the resolved fill values, never the full URL with a secret query fill.
5. **vault fill requires a `tool → vault (read)` wire**, resolved at call time; removing the wire breaks
   resolution; the vault value never appears in `/v1/tools/:id/ops`, the MCP schema, or `/v1/board`.

## C. Fill override / schema confinement (§3d rule 1-2)
1. `wheel tool ls` / MCP schema expose ONLY `agent`-mode fields. The engine REJECTS any arg key that is not
   an agent-mode field (400, logged as a denial) — a caller cannot supply a value for a `static`/`vault`/
   `hidden` field. **Test:** supply the name of a vault-filled header in `args` → 400, and the vault value
   still authoritative (not overridden).
2. **Precedence is absolute:** `vault`/`static` win; an agent can never override them via a duplicate key,
   case variant (`Authorization` vs `authorization`), array-vs-scalar, or a JSON-pointer/dotted-path collision
   in a body fill. **Test all four** against header, query, and body (json-pointer) fills.
3. **Body fills** (`application/json` json-pointer, form-urlencoded, multipart, text/plain): vault-in-body,
   duplicate-pointer override, a pointer that escapes into a sibling object. This is the coverage gap finding
   012 flagged — the mock parsed no body; the executor must be tested here.
4. **Type coercion / injection:** agent supplies an object/array where the schema says string → must be
   rejected, not stringified into the URL/body in a way that injects structure.

## D. Header / request injection
1. **CRLF in agent-filled values** (header, path, query): `X-Foo: a\r\nX-Injected: b`, or a path/query with
   `%0d%0a`. reqwest's typed headers reject control chars — confirm the executor uses typed insertion, not
   string concatenation, for headers, and percent-encodes agent-supplied path/query segments.
2. **Host header override** via an agent-filled header named `Host` → could re-target a pinned-IP connection
   at an internal vhost. Deny agent control of `Host`.

## E. Resource limits (§3d rule 3)
30s timeout (whole request incl. redirects), response body **streamed** to a ≤5 MiB cap (not buffer-then-
check), ≤3 redirects, and a decompression cap (a 5 MiB gzip bomb expanding to GBs — cap DECODED bytes).
Large/duplicate response headers. Slowloris (a server that dribbles bytes under the byte cap forever → the
30s wall-clock timeout must cover the whole response, not just connect).

## Delivery
Staged probe `redteam/pocs/tool-exec/t_tool_executor.py` (below) runs when `POST /v1/tools/:id/call` /
`wheel tool call` lands. It stands up (in-container, RoE-compliant) a **mock metadata** server and a
**redirector** on loopback, plus an echo server, and asserts: internal targets refused (direct + via
redirect + via rebinding + via encoded host), secrets never replayed on redirect, secrets never echoed to
the agent, non-agent fields rejected, CRLF rejected, limits enforced. PASS = resisted.
