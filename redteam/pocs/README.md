# Red-team PoCs

Executable attack probes. Each encodes an attack and asserts the **secure** outcome, so a PASS = the
system resisted and a FAIL = a live finding. QA can lift any of these into a regression test.

## Rules of engagement (enforced here)
- Target ONLY the local dev stack. Point probes at it via env `WHEEL_STACK` (e.g.
  `http://localhost:8080` for the API) and `WHEEL_HOST` for the host. No probe hits a public host.
- Cloud metadata is MOCKED: `redteam/pocs/mocks/metadata.py` stands up a fake `169.254.169.254`
  responder on localhost; SSRF probes target that, never the real link-local address.
- Read-only outside `redteam/`. Probes never mutate product code or tests.

## Layout
- `lib/harness.py` — shared client + helpers: JWT minting (valid + attack variants), header builders,
  HTTP with no-follow-redirect, skip-if-no-stack guard.
- `mocks/metadata.py` — fake metadata + a DNS-rebinding style flip target.
- `<campaign>/` — one dir per campaign; files named `t_<attack>.py`. Each is standalone
  (`python3 t_x.py`) and prints `PASS: resisted` / `FAIL: <finding>` with a non-zero exit on FAIL.

## Status
Skeletons are marked `PENDING-STACK` and skip cleanly until `WHEEL_STACK` is set and reachable.
They are written now (M0/M1) so the tenancy/proxy/SSRF campaigns run the moment compose boots.

## Campaigns → findings
- `envelope-forgery/`  → findings/001  (unit-testable against wheel-core once it builds)
- `api-tenancy/`       → (owner API)   IDOR + JWT
- `proxy-ingress/`     → (owner API)   path smuggling / route confusion
- `engine-wire/`       → (owner SDK)   token/wire enforcement, SQL/chest escapes
- `child-isolation/`   → findings/002,003  non-root, /proc, /data, egress
- `tool-ssrf/`         → findings/004  resolve-and-pin, redirect recheck, IP encodings
- `delegation/`        → findings/006  grant/place attenuation (§3e)
