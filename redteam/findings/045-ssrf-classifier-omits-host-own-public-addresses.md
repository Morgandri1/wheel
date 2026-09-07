# 045 — SSRF classifier denies private RANGES but not the host's own PUBLIC addresses (§3d(4) deviation)

- **Severity:** Low (defense-in-depth; no exploitable path found today — the practical routes are already
  closed by other controls). Owner: SDK/Engine. Boundary TB1/TB4 (agent-influenced URL → the host itself).
  Residual from the SSRF audit whose main verdict is SOUND (33/33 battery proven by run; PoC in
  redteam/pocs/ssrf).
- **Status:** CONFIRMED by source. The classifier `wheel_core::ip_is_denied` denies address RANGES
  (loopback, RFC1918, link-local, CGNAT, benchmarking, 0/8, 240/4, unique-local, IPv6-embedded-IPv4), and
  `host_is_denied` denies `*.railway.internal` / `*.internal` pre-DNS — but neither denies the host machine's
  OWN routable/public addresses.

## What §3d(4) requires vs what is implemented
The SSRF policy (§3d rule 4) lists what must be denied: "loopback, RFC1918, link-local, `*.railway.internal`,
`*.internal`, and **the host's own addresses**." The classifier implements every item EXCEPT the last: it
reasons about address CLASSES, not about "is this address one this machine is bound to." An SSRF to the host's
own PUBLIC IP (or a public name that resolves to it) passes `ip_is_denied` — it is a public address.

## Why it is Low, not higher (the practical routes are already closed elsewhere)
- The engine control plane is a unix socket (process mode) — not IP-reachable at all — or `:7000` bound inside
  the sandbox (docker mode).
- The host API (`:7100`) is bearer-gated (`WHEEL_HOST_SECRET`); a tool call carries no such bearer, so a hit
  lands on a 401, not a control-plane action.
- `*.railway.internal` (the private-network name for the host and Postgres) is denied pre-DNS by
  `host_is_denied`, and cross-container docker addresses are RFC1918 → already denied.
So the uncovered case is narrow: an SSRF to the host's own PUBLIC/routable IP where a non-bearer-gated service
is bound to it. On the current topology nothing sensitive is, so there is no live exposure — but the control
is "safe because of what happens to be bound," not "safe because the policy the contract states is enforced,"
which is the same gap-shape as 044 (safe by build flag, not by the authorizer).

## Fix (SDK) — make the stated policy true
Deny the host's own addresses explicitly, so the guarantee does not depend on what is bound:
1. At engine/host startup, enumerate the machine's own addresses (`getifaddrs`/equivalent) and add them to the
   deny set consulted by `resolve_for`/`first_denied` — every resolved address AND every redirect hop, same as
   the range checks.
2. Belt: keep every host-internal service bound to loopback or a unix socket (the engine control plane already
   is; assert the host API is not reachable on a public bind without the bearer).
3. Add the host's-own-address case to the SSRF test battery so the §3d(4) list is enforced by a test, not by
   deployment topology.

## Note
Filed at PM's direction as 045 (number assigned by PM to avoid colliding with the 044 remediation). The parent
SSRF audit is SOUND; this is the one item on the §3d(4) list the classifier does not yet enforce, mitigated in
practice but worth closing so the policy is true by construction.
