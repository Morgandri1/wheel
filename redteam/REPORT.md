# Red-team report — Wheel

System of record (§3c#15): this file + `redteam/findings/*` + `redteam/reviews/*`. Re-read on rebase;
messages are only notifications. Last updated: pre-M1 (no bootable stack yet — all findings are design/
code review; PoCs in `redteam/pocs/` flip to CONFIRMED/CLOSED the moment `wheel-host` docker + compose boot).

## Open findings by severity

| # | Sev | Title | Owner | Status / ruling |
|---|-----|-------|-------|-----------------|
| 001 | Critical | `<AgentPrompt>` envelope forgery — body-injected 2nd envelope forges attribution → bypasses wire matrix | SDK | ACCEPTED; normative escaping algo in PROTOCOL.md (escape opening AND closing tag, case-insensitive, decoded bytes; engine-set attrs). Verify by shared fixture. |
| 003 | Critical | Process backend: one kernel+container for all tenants; uid/egress are the only isolation | API+SDK | Test matrix pre-written; verify when process backend lands (M3). |
| 002 | High | bypassPermissions = agent is untrusted RCE by design → all enforcement engine/kernel-side | SDK | ACCEPTED as a §2 principle; children run non-root + IS_SANDBOX=1. + token-type discrimination nuance (from api review). |
| 004 | High | Tool-node SSRF + arbitrary-agent egress gap | SDK+API | ACCEPTED (resolve-and-pin, per-redirect re-validate, IP-encoding normalise). Confirm-first vector: redirect + DNS-rebind. Probe staged. |
| 005 | High | Built-in MCP server = 2nd capability entrypoint | SDK | ACCEPTED; one shared authz fn for MCP + `/v1/cli/*`; SSRF policy applies to mcp.url. |
| 006 | High | Capability delegation (grant/place/manage) escalation — attenuation must hold | SDK+API | OPEN (§3e, M3). Granted wire ≤ grantor's effective wire; owner-only; wheel.toml import reuses create-validation. |
| 007 | High | Per-node token/creds isolation collapses within a project (same uid) | SDK+API | ACCEPTED → **per-node uid** (project uid range, ambient CAP_SETUID/SETGID only, 0700 creds, WHEEL_TOKEN via 0600 file). M2/M3. |
| 009 | High | Node-config validation collapsed to ONE layer, and that layer is untested | SDK | OPEN. Schema accepts 12 forbidden configs (QA BUG-001); validate.rs 73% / state.rs 0% / ws 67% vs 90% mandate. Scoped: not "engine accepts .." — unverified. |
| 008 | Medium | Child stdout is attacker-controlled → forgeable `result`/`usage`/`session_id` | SDK | ACCEPTED; budget/turn + session_id enforced supervisor-side; test a fake `result` can't reach the parser top-level. |

## Systemic themes
1. **Attribution & control-stream integrity (001, 008).** The `<AgentPrompt>` envelope and the harness
   stdout stream are the only primitives that say *who is speaking* and *what the engine should do*. Body
   text and child-controlled fds must never be able to forge either. This is the single most load-bearing
   parsing invariant in the system.
2. **Isolation on a shared kernel (002, 003, 007).** Every agent is untrusted RCE (bypassPermissions), and
   in prod all tenants share one kernel + one container. Isolation reduces to per-node uid + fs perms +
   egress filtering — controls that must be explicit and tested, never assumed. PM's per-node-uid ruling
   (007) is the right call; its tests are the proof.
3. **Defence-in-depth that is actually two layers (009, 001).** The contract promises "rejected by engine
   AND api." Today the schema layer is loose (12 configs) and the engine layer is untested (<90%). A promise
   of two defences with one unverified defence is one defence.
4. **Server-side enforcement, everywhere (004, 005, 006, 002-nuance).** Every egress (tool/mcp/script),
   every capability grant, every CLI/MCP call, and every token type must be re-validated at the engine —
   the agent, the proxy, and the message body are all untrusted. One shared authz choke point, not many.

## Top-3 recommendations
1. **Make the two validation layers real and tested** (009/001): strict JSON Schema
   (`additionalProperties:false`, `pattern` on `endpoint.path`, mcp transport `oneOf`, `timeout` bounds) +
   `validate.rs` ≥90 % with the 12 BUG-001 fixtures as negative tests. Cheapest, highest leverage; restores
   defence-in-depth and closes the ingress/chest/secret static gaps at once.
2. **Land the per-node uid + 0600 token-file model** (007) with the cross-uid `/proc` and token-file EACCES
   tests at M2 (docker) — the highest-impact isolation control, and the thing the entire per-node wire/grant
   model rests on at runtime.
3. **One shared server-side authz function for CLI + MCP**, with envelope escaping (001), token-type
   discrimination (002 nuance), and full-matrix allow/deny (005) all proven by test — the choke point that
   makes "the wire is the capability" true rather than aspirational.

## Plan reviews (§0b, all complete)
- `reviews/sdk-plan-M0.md` — approved; produced 007, 008 + must-verify (SQL authorizer scope, no-argv-secrets,
  auth via single-writer). One correction: script→tool is contract-allowed (QA BUG-004), deny is a PM proposal.
- `reviews/qa-plan-M0.md` — approved; added attacker-view gaps (ENG-sql-authorizer, chest-traversal,
  SUP-forged-event, ISO-uid). Shared envelope fixture handed off (now single-sourced from qa/fixtures/).
- `reviews/api-plan-M0.md` — approved; most of the TB1/TB2 attack list already closed correctly in code; a
  few must-verify-at-boot items + the engine token-type invariant routed to SDK.

## Verification queue (runs when the stack boots)
`pocs/api-tenancy` · `pocs/proxy-ingress` (encoded-slash/double-decode) · `pocs/tool-ssrf`
(redirect+rebind, confirm-first) · `pocs/engine-wire` (config-rejection for F009, SQL authorizer scope,
chest traversal) · `pocs/child-isolation` (non-root, /proc, token-file) · `pocs/delegation` (attenuation) ·
`pocs/envelope-forgery` (shared fixture, via QA fake-harness transcript).
