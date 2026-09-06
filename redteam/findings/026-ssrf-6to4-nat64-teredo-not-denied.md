# 026 — SSRF: `ip_is_denied` misses 6to4 / NAT64 / Teredo → a hostname to those ranges reaches loopback

- **Severity:** Medium (SSRF bypass to loopback/private, conditional on the host having IPv6-transition
  routing — which some cloud/Railway IPv6 egress does via NAT64; the guard must deny by construction
  regardless). Owner: **SDK/Engine** (`crates/wheel-core/src/tool.rs::ip_is_denied`).
- **Status:** CONFIRMED LIVE — `wheel-engine:dev` @ HEAD (image 00:22Z, current `ip_is_denied`). PoC:
  `redteam/pocs/tool-exec/run_tool_live.sh` (Part C). Container removed. This is the angle SDK flagged lowest
  confidence in.
- **Boundary:** TB7 (tool executor `send`) — the SSRF policy §3d rule 4.

## What
`ip_is_denied` (`tool.rs`) for IPv6 denies loopback, unspecified, ULA (`fc00::/7`), link-local (`fe80::/10`),
and IPv4-**mapped** (`::ffff:a.b.c.d`, embedded v4 re-checked). It does NOT deny the IPv6-transition ranges
that also embed a v4 address:
- **6to4** `2002::/16` — `2002:7f00:0001::` embeds `127.0.0.1`.
- **NAT64** `64:ff9b::/96` — `64:ff9b::7f00:1` embeds `127.0.0.1`.
- **Teredo** `2001::/32`.
`resolve_and_check` runs `ip_is_denied` on every resolved address (that part is correct), but these ranges
pass it, so the connection proceeds.

## Live proof (Part C — hostname path)
A base_url with an IPv6 **literal** (`http://[64:ff9b::7f00:1]/`) happens to fail earlier because
`lookup_host` chokes on the bracketed form — a fail-safe by ACCIDENT, not by the guard. The real trigger is a
**hostname** that resolves to such an address (DNS, or `/etc/hosts` here). Mapping names and comparing:
```
loopback-hostname  lo.test      -> 127.0.0.1        : DENIED-BY-GUARD  "lo.test resolves to 127.0.0.1, which is not reachable"
nat64-hostname     natsix.test  -> 64:ff9b::7f00:1  : PASSED THE GUARD -> reached connect ("error sending request...", no route here)
6to4-hostname      sixfour.test -> 2002:7f00:0001:: : PASSED THE GUARD -> reached connect
```
`lo.test` (→127.0.0.1) is denied; `natsix.test`/`sixfour.test` (→ the SAME 127.0.0.1, via NAT64/6to4) are
NOT — they reach the connect stage. On a host whose kernel has NAT64/6to4 routing (Railway and other
IPv6-egress environments can), `64:ff9b::7f00:1` routes to `127.0.0.1` → SSRF to the engine's own control
plane / co-tenant sockets on the shared host. Even without routing, the guard letting these through is a
policy hole: the SSRF deny-list exists precisely to keep loopback/private unreachable.

## Fix
Extend `ip_is_denied`'s v6 arm to deny (or extract-and-recheck the embedded v4 of):
```rust
// 6to4 2002::/16 — embedded v4 is s[1..3]
|| s[0] == 0x2002
// NAT64 64:ff9b::/96 — embedded v4 is the last 32 bits
|| (s[0] == 0x0064 && s[1] == 0xff9b)
// Teredo 2001::/32 (2001:0::/32)
|| (s[0] == 0x2001 && s[1] == 0x0000)
```
Best: for 6to4/NAT64, pull out the embedded IPv4 and run `ip_is_denied(v4)` (so a 6to4 wrapping a PUBLIC v4
is still allowed, but one wrapping loopback/private is denied) — mirrors the existing IPv4-mapped handling.
Add a test: `natsix.test`→`64:ff9b::7f00:1` and `sixfour.test`→`2002:7f00:1::` must be DENIED like `127.0.0.1`.
(Also worth: normalise/strip brackets so an IPv6 literal is validated by IP rather than incidentally failing
`lookup_host`.)

## Same-run confirmations (NOT findings)
- **022 fixes VERIFIED e2e** (dry_run curl): query vault → `key=<redacted>`, path static → `x/<redacted>`
  (encoded-form masking works), cookie `x; admin=1` → `sid=x%3B%20admin%3D1` (`;`/`=`/SP encoded, no cookie
  injection). Both 022 findings are fixed.
- **SSRF regressions hold:** loopback / RFC1918 / `169.254.169.254` / `*.railway.internal` denied at config;
  decimal `2130706433` → resolved to 127.0.0.1 → denied at call; every-resolved-address + pin correct (read).
- **merge_operations un-pin (024) holds:** `reimport` returns 409 `would_unpin` unless `allow_unpin` for a
  renamed param, a location change, a method/path change, or a removed op; `same_param` is case-insensitive
  so a case-only rename still matches. Tool routes are engine-secret realm (not agent-reachable; 025).
