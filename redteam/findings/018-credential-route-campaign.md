# 018 — Credential routes (auth/complete setup_token|api_key + save_to_vault, paste-code session): RESISTED

- **Severity:** Informational (no vuln). Verified-secure record.
- **Owner:** SDK/Engine (`api/agent_routes.rs` auth_complete/finish_setup_token/finish_paste_code/
  save_credential_to_vault, `api/vault_routes.rs` store_in_vault, `oauth.rs` LoginRegistry, `auth.rs`
  classify_token).
- **Status:** VERIFIED RESIST — live against a **freshly rebuilt** `wheel-engine:dev` @ HEAD (image 22:38Z;
  the setup_token/save_to_vault code is 08b91f1 @ 22:22, so a rebuild was required — see note).
  PoC: `redteam/pocs/credential/run_credential_campaign.sh` + `t_credential_routes.py`. Container removed.
- **Boundary:** TB1/TB4/TB5/TB6 (auth surface, node CRUD, child↔engine, child↔CLI).

## Result — 17/17 resisted
| target | result |
|--------|--------|
| 1 setup_token refuses non-durable | api03 key, `sk-live-…`, ``, `oat`, `SK-ANT-OAT…` (upper), ` sk-ant-api03…` (space) → 400; only `sk-ant-oat…` accepted (200, no echo) |
| 2a save_to_vault requires a read wire | agent with no wire → 403 `wire_denied` |
| 2b save_to_vault honours ambiguity | co-reader collision → 409 `ambiguous_credential` (shared `store_in_vault` choke point) |
| 2c clean save | 200; body = `{authenticated,mode,vault:{key:CLAUDE_CODE_OAUTH_TOKEN,stored}}` — **token value never echoed** |
| 3a board | `/v1/board` carries no credential value |
| 3b GET auth | no value; after vault-save → `{authenticated:true, mode:"env", source:"v3"}` (source = vault NAME) |
| 3c wired readback | the wired agent's `wheel secret get v3/CLAUDE_CODE_OAUTH_TOKEN` returns the value (intended, only path) |
| 3d sibling readback | an unwired sibling agent → 403 `wire_denied` (no cross-agent read) |
| 5 GET auth shape | keys ⊆ {authenticated,mode,source,account,expires_at,warning}; never the value |
| F1 session fixation | complete with a WRONG session id → 409 `expired` (child killed) |
| F2 replay | second complete → 409 (NoSession; pending removed at entry) |

## Why it holds (inspection, confirmed live)
- `classify_token` gates setup_token to the `sk-ant-oat` prefix (`finish_setup_token` rejects anything else),
  so a session/provider token cannot be vaulted as "durable" for five agents to depend on.
- `save_credential_to_vault` requires a `read` wire (403 else) and routes through the SAME `store_in_vault`
  the vault PUT route uses — so the ambiguity rule (per-reader, across ALL readers of the target vault) is
  enforced on this path too, not bypassed. The response is metadata only.
- `LoginRegistry.complete(node, session, code)` is keyed by the agent node and `remove`s the pending at
  entry: replay → NoSession; a mismatched `session` → Expired (child killed); TTL (`SESSION_TTL` 15m) +
  `arm_expiry` reap the child. No cross-agent session drive.
- Credential values reach only the wired agent's env / wire-gated `secret_get`; never board, logs, WS, or the
  AuthStatus body (redaction remains accidental-echo only, per finding 016 — not a boundary vs a hostile agent).

## Notes for the record
1. **Stale-image trap (again).** The first run reported 5 "findings" that were all a stale `wheel-engine:dev`
   (21:29Z) predating setup_token (22:22Z) — it ignored the new fields and returned the old
   "supply either api_key or code" 400. Rebuilt (`make engine-image`) → re-ran → clean. Standing lesson:
   **check the image build time against the commit under test before trusting a result.**
2. **Test-rig correction (engine more correct than the test).** An initial "clean save" FAIL was my board:
   agent A was wired to both v1 and v2, so adding `CLAUDE_CODE_OAUTH_TOKEN` to v1 correctly raised ambiguity
   for A (a co-reader of v1). The engine enforces ambiguity across every reader of the target vault, not just
   the caller. Fixed by saving into a vault only the caller reads (v3). Not a vuln — a stronger guarantee.

## Minor (non-security) note to SDK
`save_credential_to_vault` always stores under `KEY=CLAUDE_CODE_OAUTH_TOKEN`, even for the `api_key` path
(a provider `ANTHROPIC_API_KEY`, or a codex `CODEX_API_KEY`). A provider key vaulted under
`CLAUDE_CODE_OAUTH_TOKEN` would be exported to peers under the wrong variable and fail auth (a foot-gun,
not a leak). Consider keying by the credential's own env var (`token_env(kind,harness)`).

## Follow-up (SDK's two post-fix targets) — source review, NO agent-reachable finding
**auth/complete false-"rejected":** `oauth.rs complete()` captures `before = output.len()` immediately
before writing the code, then `verdict(&mut pending, before)` scans ONLY output produced after `before`.
So the greeting / authorize-URL (printed pre-submission, even if it contains "invalid"/"denied") cannot
match a REJECTION_MARKER. A marker appearing post-submission would be the CLI's own response — not
board-agent-controllable — and a false match only DENIES a login (fail-safe direction), never grants one.
Defended.

**vault expires_at hostile values:** the spawn-time gate (`supervisor::lapsed_credential`) reads the VAULT
expiry as `wheel_core::Timestamp` (`credential_detail`→`expiry_of`), which is an RFC3339 value parsed with
`?` at write time — an unparseable/garbage expiry is rejected when stored, not at spawn. `into_inner()` is a
total function on an already-valid `OffsetDateTime` (no i64→time overflow/panic path here — the i64-millis
`StoredOauth.expires_at` in auth.rs is response METADATA, not a time gate). A past/negative expiry →
`<= now` → agent refuses to start with `needs_auth` (fail-CLOSED, recoverable by re-auth), not a permanent
brick. And vault writes are host-realm (`PUT /v1/vault`, engine secret) storing a RAW STRING value — an
agent cannot set a structured expiry through any route, so this is not agent-reachable. No finding.
One thing for SDK to confirm (couldn't see it fully): which route calls `put_with_expiry` (i.e., is a vault
expiry ever set from a value an agent influences?). If none, target 2 is moot for the agent threat model.
