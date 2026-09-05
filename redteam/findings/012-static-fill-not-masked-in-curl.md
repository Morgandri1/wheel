# 012 — `static` fills rendered in cleartext in copy-as-curl / call url (vault is masked)

- **Severity:** Medium (S2 — contract violation + latent agent-facing secret leak)
- **Owner:** Web (mock, `web/mock/server.ts`) now; SDK/Engine when the importer lands (same logic must not port the bug)
- **Status:** CONFIRMED against the mock (web/main 85a4028). PoC: `redteam/pocs/tool-fills/t_curl_static_leak.mjs`.
- **Boundary:** TB7 (tool nodes / fills), finding 004 family.

## Claim tested (PM's 4)
"curl renders `****`" — TRUE for **vault**, FALSE for **static**. `renderCurl` (server.ts:596) masks
`vault → "****"` but renders `static → param.fill.value` verbatim, and `renderUrl` (server.ts:574)
writes static path/query values in cleartext into the URL, which is returned in the call `body.url`
(server.ts:329) and embedded in the curl.

## Contract violated
- §3d rule 3 (ARCHITECTURE.md:260): "`--curl` / the UI 'copy as curl' render the exact equivalent
  `curl` with **static/vault values masked**." BOTH must be masked; static is not.
- §3d fill semantics: `static` is "a value the user typed; **never shown to the agent**." When the
  engine reuses this rendering for the agent-facing `wheel tool call <op> --curl` (dry_run — which the
  call handler already serves after allowing only agent-mode fields), the agent receives static values
  in cleartext, learning a secret the user chose to pin.

## PoC (verbatim renderUrl/renderCurl, run under node)
```
curl -X GET -H 'X-Vault-Key: ****' -H 'X-Static-Token: SECRET-STATIC-abc123' \
  'https://api.example.com/data/acme-secret-tenant?region=eu-secret-1'
```
vault masked; static header `SECRET-STATIC-abc123`, static path `acme-secret-tenant`, static query
`eu-secret-1` all cleartext. `node redteam/pocs/tool-fills/t_curl_static_leak.mjs` → exit 1.

## Impact
1. Owner's copy-as-curl is a shareable/screenshot/log-able string that now carries pinned static
   secrets (people DO paste API keys as `static` when they don't realise `vault` exists).
2. Latent: the engine's agent-facing `--curl` reusing this leaks static to the agent, breaking the
   "static never shown to the agent" guarantee — an agent info-leak, not just a UI blemish.

## Proposed fix
In BOTH `renderCurl` and `renderUrl`, mask non-agent authoritative values the same way for static as
for vault (render `****` / `<static>` placeholder), not the value. One-line parity:
```js
const value = (mode === "vault" || mode === "static") ? "****" : String(args[param.name] ?? "");
```
(and the equivalent in renderUrl for path/query). Engine importer must land with the same masking on
its `--curl` path, and must NOT expose static values to the agent via dry_run.

## The other three claims — VERIFIED RESIST (mock, params only)
- **vault-pinned header absent from agent schema:** `agentInputSchema` (tools.ts) skips every
  non-agent fill → PASS.
- **vault key name never in ops response:** `/v1/tools/:id/ops` returns `agentInputSchema(op)`
  (server.ts:293), which emits only agent-mode name+schema — the vault ref never appears → PASS.
- **agent-supplied pinned field → 400:** the call handler rejects any `args` key not in the agent-mode
  set (server.ts, "fields not open to the caller") → PASS.

## Coverage gap (not a vuln, flag for the engine)
The mock parses NO request body, so all four claims are verified for path/query/header/cookie params
only — never for request-BODY json-pointer fills, which is exactly where §3d's vault-in-body and
duplicate-/case-variant-pointer override attacks live. The mock fail-closed-rejects body args today.
These claims MUST be re-attacked against the engine importer for body fills.
