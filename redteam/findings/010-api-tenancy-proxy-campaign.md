# 010 — API tenancy + proxy/ingress campaign: VERIFIED-SECURE (no vuln) + 2 must-verify

- **Severity:** Informational (no vulnerability found). 2 non-vuln must-verify items → API.
- **Owner:** API
- **Status:** RESISTED — run live against infra/dev (main @ origin, WHEEL_ENV=dev), 2026-09-05.
- **Boundary:** TB1 (browser↔API), TB2 (proxy/ingress).
- **PoC:** `redteam/pocs/proxy-ingress/live_campaign.py` (reusable as a QA regression; 22 assertions).

## Result: 22 attacks resisted, 0 findings
Ran PM's full list against the running stack. Every attack was correctly refused. This is a
verified-secure record, filed per §3c#15 so the negative result survives as evidence.

| Attack | Observed | Verdict |
|--------|----------|---------|
| Path traversal `..` (proxy) | 400 `path traversal is not permitted` | resisted |
| `%2e%2e`, `%2f` encoded traversal | 400 | resisted |
| double-encoded `%252e%252e` | 404 (not reconstructed) | resisted |
| `/v1/v1/board` double-prefix (PM's smell) | 404 not_found (forwarded, engine 404) — no leak | resisted |
| escape to host root / api's own `/v1/projects` via proxy | 400 | resisted |
| backslash `..\..\` | 404 | resisted |
| `authorization` header smuggled upstream | scrubbed; board still 200 (engine got the real secret) | resisted |
| `x-project-id` ≠ path | 400 `x-project-id does not match the project id in the path` | resisted (fail-closed) |
| `x-project-id` = garbage | 400 `must be a valid uuid` | resisted |
| WS upgrade on non-WS route | 502, NOT 101 (no protocol switch to client) | resisted (+ see obs. 2) |
| JWT alg=none / wrong secret / empty secret | 401 | resisted |
| JWT expired / future nbf / wrong iss / tampered payload | 401 | resisted |
| ingress body > 5 MiB (http cap on) | 413 | resisted |
| ingress rate limit under 80-burst | 21/80 → 429 | resisted |

Cross-user 404 (proxy + `GET /v1/projects/:id`), no-token → 401, ingress-off → 403 were already
proven by API's e2e; re-confirmed incidentally.

## Not exercised in dev (prod-only)
- **RS256→HS256 confusion, unknown-kid JWKS flood:** dev auth is static HS256 (no JWKS), so these are
  unreachable here. API's own suite covers them with a mocked JWKS — not re-proven, not a gap.

## Must-verify → API (non-vuln)
1. **`POST /v1/projects/:id/ws-ticket` returns 404 (route unimplemented in this dev build), and
   `…/engine/v1/events` 404s.** The contract (§5) requires the single-use, 30 s, (user,project)-bound
   ticket because browsers cannot set an auth header on a WS handshake and the JWT must never be in a
   URL. Not a vuln today (nothing authenticates via the URL yet), but the browser-WS auth model is
   unresolved. When implemented, it MUST be single-use + expiring + project-bound — I will attack
   replay/cross-project/expiry then. Flagging so it isn't shipped as a plain JWT-in-URL.
2. **WS-vs-HTTP is decided purely from client `Upgrade`/`Connection` headers**, so any engine route can
   be coerced onto the WS-bridge path (observed: `/v1/board` + upgrade headers → 502 from the bridge,
   not a client protocol switch). Harmless against the stub; confirm the real engine's non-WS routes
   can't be driven into a half-open bridge that ties up a connection (slowloris-style) — a bounded
   handshake timeout on the bridge covers it.

## Note
`/v1/cli/*` through the authenticated proxy: the proxy forwards it and the (bearer-gated) stub returned
404 — i.e. the host/engine bearer DOES reach that path. Not an API bug (proxy correctly forwards); the
token-type discrimination is the engine-side invariant already routed to SDK (findings 002 #2 / 005).
