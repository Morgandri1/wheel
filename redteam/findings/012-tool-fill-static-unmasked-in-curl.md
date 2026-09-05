# 012 — `static` fill values not masked in curl render (agent-reachable leak)

- **Severity:** Medium (agent-facing info leak + §3d rule-3 divergence)
- **Owner:** Web (mock, now) → SDK/Engine (the real `--curl` renderer when the importer lands)
- **Status:** CONFIRMED against web/main mock (85a4028+), 2026-09-05. Re-test the engine renderer when it lands.
- **Boundary:** TB5 / §3d tool-node fills.

## Claim (scoped)
§3d rule 3: "`--curl` / the UI 'copy as curl' render the exact equivalent `curl` with **static/vault**
values masked." §3d also defines `static` = "a value the user typed; **never shown to the agent**."
The web mock's `renderCurl` (web/mock/server.ts) masks ONLY `vault` and renders `static` in
CLEARTEXT — so a static fill leaks wherever curl is rendered, including the agent-reachable `--curl`.

## Repro (confirmed)
The mock renderer:
```js
value = mode === "vault" ? "****"
      : mode === "static" ? (param.fill?.value ?? "")   // <-- cleartext
      : String(args[param.name] ?? "");
```
Driving it with a vault header + a static header:
```
CURL: curl -X GET -H 'X-Vault-Key: ****' -H 'X-Static-Secret: sk-STATIC-SECRET-1234'
vault masked? true      static LEAKED? true
```
`renderUrl` has the same asymmetry for path/query params (vault→`<from vault>`, static→cleartext),
so a static query value leaks in the returned `url` too.

## Why it matters
- Contract divergence: rule 3 says static AND vault are masked; the renderer masks only vault.
- Agent-facing leak: the contract's own CLI (`wheel tool call <tool> <op> --curl`) renders via this
  path. `static` is defined as never shown to the agent, yet an agent invoking `--curl`/`dry_run`
  reads it verbatim. A user who pins a secret as `static` (a reasonable mistake — the UI accepts any
  string) hands it to every agent wired to the tool.

## Proposed fix
Mask `static` exactly as `vault` in both `renderCurl` and `renderUrl` (→ `****` / `<static>`), for
the agent-facing `--curl`/`dry_run` path. If the owner-facing UI wants to reveal a static value, that
is a separate, explicitly owner-only affordance — never the default renderer the agent can reach.
When the engine importer lands, its `--curl` MUST mask static; I will re-run this against it.

## Secondary (same file, lower confidence — must-verify at the engine)
1. **CRLF not sanitised in agent-supplied header values.** `renderCurl`/`renderUrl` interpolate
   `args[name]` into `-H '<name>: <value>'` with no CR/LF stripping. In the mock this only malforms a
   copied curl string, but the ENGINE must strip CR/LF before setting real headers or it is header /
   request-splitting injection (finding 004 vector). Verify at the engine executor.
2. **Body fills unmodeled.** `parseSpec` handles only path/query/header/cookie params, not request
   BODY fills (§3d `body.fills`, json-pointer). So the "agent schema", the extra-field 400, and the
   masking are NOT exercised for body fields anywhere in the mock. The engine must apply all four fill
   rules to body fields too; the mock can't prove it. Coverage gap, not a mock bug.

## Verified-correct in the same mock (credited)
- `agentInputSchema`: non-agent (static/vault/hidden) params are absent → agent can't see or supply
  them (claim 1 ✓); vault ref/key name never emitted to the agent schema (claim 2 ✓).
- call handler: an agent-supplied field not in the agent-mode set → **400** "fields not open to the
  caller" (claim 4 ✓); case-variant of a pinned name is rejected as an extra field, not merged.
- `mergeOperations`: re-import keeps prior fills (§3d rule 5).
