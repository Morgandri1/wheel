# Review record — `save_to_vault` credential path (contract-mandated red-team review)

`save_to_vault` takes ONE node's credential and hands it to MANY (every agent reading the vault). That
fan-out makes it the highest-leverage surface on the board, so **every change to this path requires a
red-team review** (PM contract rule, at SDK's request). This is the canonical record; the live evidence is
in findings 018 and 021.

## Path
`POST /v1/agents/:id/auth/complete {code|api_key|setup_token, save_to_vault?}` (engine-secret/host realm)
→ `save_credential_to_vault(agent, vault, found)` → requires a `agent → vault (read)` wire → shared
choke point `vault_routes::store_in_vault` → `vault::put_with_expiry`. Recognised credential keys are
routed by `classify_token`/`token_env` (fixed in a1bf3be).

## What has been reviewed and VERIFIED (findings 018, 021)
- **Wire-gated:** no read wire → 403 `wire_denied`. (018 §2a)
- **Ambiguity enforced** via the SAME `store_in_vault` as the vault PUT route — per-reader, across ALL
  readers of the target vault → 409 `ambiguous_credential`, not bypassed by this path. (018 §2b, live)
- **No value echo:** response is metadata only (`{name,key,stored,expires_at?,warning?}`); the token never
  appears in the response, `/v1/board`, logs, WS, or the AuthStatus body. (018 §2c/§3)
- **Readback scoping:** the stored credential is readable only by the wired agent's env / wire-gated
  `secret_get`; a sibling agent with no wire → 403. (018 §3c/3d)
- **setup_token durability:** the `setup_token` field REFUSES a non-durable credential (only `sk-ant-oat`),
  so a session token can't be vaulted as durable for peers. (018 §1)
- **Correct key routing:** now keyed on `classify_token`/`token_env` so the vault and the child env can't
  disagree (was hardcoded `CLAUDE_CODE_OAUTH_TOKEN` — the bug 018 flagged, fixed a1bf3be).
- **Session fixation / replay:** the paste-code `LoginRegistry` is keyed by node, removes the pending at
  entry (no replay), rejects a mismatched session, TTL-reaps. (018 F1/F2)
- **auth/complete false-"rejected" (SDK target):** the rejection matcher scans only post-submission output
  (`before = output.len()`), so the greeting/authorize-URL can't false-match; a false match only DENIES a
  login (fail-safe). Defended. (018 follow-up)
- **Hostile `expires_at` (SDK target):** `millis_to_timestamp` is i128 + range-checked → overflow yields
  `None`, no panic; a past value → the reader refuses to start (`needs_auth`, recoverable). (021)

## Open sharp edge (Low, hardening — finding 021)
A SESSION credential saved via the paste-code path into a SHARED vault carries its expiry, and when it
lapses `lapsed_credential` strands EVERY reader (blast radius = all peers), not just the author — the path
only WARNS. Planting is **operator-only** (agents have no vault write; `PUT` carries no expiry), so it is
**not agent-reachable**, but the foot-gun is real. **Recommendation:** when `save_to_vault` targets a vault
with any reader other than the caller, REFUSE a non-durable credential (or require explicit override),
mirroring the `setup_token` stance — close it by construction, not by an ignorable warning.

## Checklist for any future change to this path (run before merge)
1. Wire check still present and fail-closed (403 without a read wire).
2. Routed through `store_in_vault` so the ambiguity rule can't be bypassed; per-reader, all readers.
3. Response/board/logs/WS/curl carry NO credential value (static AND vault masked).
4. Credential key routed by `classify_token`/`token_env` — vault and child env cannot disagree.
5. No new way for an AGENT (node-token realm) to reach this path or to write a vault value/expiry.
6. Shared-vault durability: a non-durable credential into a multi-reader vault is refused or loudly gated.
7. Re-run `redteam/pocs/credential/run_credential_campaign.sh` (expects 17/17) against a FRESH image.
