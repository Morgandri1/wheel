# 009 — Node-config validation has collapsed to one layer, and that layer is untested

- **Severity:** High (design/assurance; blocks the "rejected by engine AND api" contract guarantee)
- **Owner:** SDK/Engine (validate.rs coverage + schema strictness) · QA corroborates
- **Status:** OPEN — pre-runtime. Built on QA BUG-001 + QA's `cargo llvm-cov` numbers (main, today).
- **Boundary:** TB2 (ingress `endpoint.path`), TB4 (vault values in config, chest/table keys), TB5 (mcp transport).

## Precise claim (scoped — do not overclaim)
The contract (§3) requires forbidden node configs to be rejected by **two independent layers**: the API
(validating against the exported JSON Schema, also Web's type source) and the engine (`wheel-core`
`validate.rs` + serde). Two facts, each established by QA:
1. **The schema layer is not doing the job.** `docs/schema/*.json` accepts 12 configs the contract forbids
   (QA BUG-001, S2) — including `..` in `endpoint.path`, vault VALUES inside a vault config, unknown keys,
   `mcp` with both `command` and `url`, and `script.timeout_secs` > 300. All twelve are expressible in
   JSON Schema; this is strictness, not capability.
2. **The surviving layer has no evidence behind it.** `validate.rs` — the code that would have to enforce
   those rejections — is at **73.38 % line coverage**; `state.rs` at **0.00 %**; whole workspace **67.46 %**
   against the §0b mandate of **90 %** (`cargo llvm-cov --workspace`, main, reported by QA to SDK+PM).

What this does **NOT** say: that the engine accepts `..`. `validate.rs` / `deny_unknown_fields` may well
reject every one of the 12 at runtime — **nobody has verified it yet.** That is the finding: a contract that
promises defence-in-depth currently has ONE defence, unspecified by the schema and substantially untested.

## Why it is High, not S4 "loose schema"
- If `validate.rs` misses `endpoint.path` `..`, ingress traversal (TB2) has zero static defence.
- If it misses vault values in config, a secret lands in a plain config row / `/v1/board` response (TB4).
- If it misses `mcp` `command`+`url` or unbounded `timeout_secs`, finding 005/DoS surfaces widen.
Each is a single-point failure with no schema backstop and no test proving the point holds.

## Proposed action
- **SDK:** (a) make the schema strict (`additionalProperties:false`, `pattern` on `endpoint.path`, `oneOf`+
  `required` for mcp transport, `min/max` on `timeout_secs`, no state fields) so layer 1 exists again;
  (b) bring `validate.rs` to ≥90 % with the 12 BUG-001 fixtures as negative tests — that is the evidence
  the second layer holds; (c) close 001/003 (ToolConfig `kind` + nested `source`) at the same time.
- **QA (agreed):** prioritise engine-side assertions for these first when `wheel-engine:test` lands, so
  the second layer gets a definitive yes/no early: **`endpoint_traversal`, `vault_with_values`,
  `config_unknown_key` (mass-assignment surface), `mcp_both`** — then the rest.
- **Me:** `redteam/pocs/engine-wire/` grows a config-rejection probe posting the 12 fixtures to
  `POST /v1/nodes` and asserting 400 on each. Status flips to CONFIRMED or CLOSED on that result.

## VERIFIED (live, 2026-09-05, docker-backend engine wheel-engine:dev @ :8080 via proxy)
Ran the 12 engine-enforced BUG-001 fixtures at `POST /v1/projects/:id/engine/v1/nodes`. **All 12
REJECTED** — the surviving layer HOLDS:
- 422 (serde `deny_unknown_fields` / unknown variant / missing field): config_unknown_key,
  state_in_config, vault_with_values, mcp_both, mcp_http_no_url, mcp_stdio_no_command,
  endpoint_auth_bad_mode, endpoint_auth_bearer_no_ref.
- 400 (explicit validate.rs): endpoint_no_slash, endpoint_traversal ("must not contain .."),
  script_zero_timeout, script_timeout_over_max ("must be between …").

**Re-scoping:** the earlier concern ("one untested layer that might fail") is retired — the engine
layer is now verified to reject every one. The residual is narrower and lower: the *schema* half
(docs/schema/*.json, Web's type source + any API-side schema check) is still loose (QA BUG-001), so
defense-in-depth is ASYMMETRIC (strong engine, loose schema), not absent. Fix the schema so a client
that trusts it can't build a forbidden config, but there is **no runtime bypass** via the engine.
Severity effectively drops to the schema-strictness issue QA already tracks as BUG-001 (S2). PoC:
`redteam/pocs/proxy-ingress/live_campaign.py`-style; reproduced inline. Coverage bar (validate.rs 73%)
is now an assurance gap for SDK, not evidence of a hole.
