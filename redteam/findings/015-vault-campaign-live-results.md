# 015 — Vault node + env-delivered credentials (M1.6): live campaign — RESISTED

- **Severity:** Informational (no finding). Two by-design residuals restated below so they are on record, not missed.
- **Owner:** SDK/Engine (vault.rs, api/vault_routes.rs, api/cli_routes.rs, supervisor/mod.rs).
- **Status:** VERIFIED RESIST — live against `wheel-engine:dev` @ 39cbcb0 (image built 2026-09-05T16:55Z).
  PoC: `redteam/pocs/vault/run_vault_campaign.sh` + `t_vault.py`. Container removed after (trap).
- **Boundary:** TB4/TB5 (engine wire enforcement; child ↔ CLI), TB6 (child-process isolation, env).

## Result — 13/13 checks resisted
| # | target | result |
|---|--------|--------|
| 1 | encrypted at rest | plaintext `SECRET1` grep of `/data/wheel.db` → **0 hits** (AES-256-GCM, per-value nonce, AAD=`node/key`) |
| 2a | `/v1/board` carries no value | 200, secret absent |
| 2b | `GET /v1/vault/:id` = names only | `{"keys":["ANTHROPIC_API_KEY","K1"]}`, no value/`value` field |
| 3a | wired agent reads its own value | 200 (intended allow) |
| 3b | agent denied on a vault it is NOT wired to | 403 `wire_denied` (exit 3), not an empty answer |
| 3c | unwired agent gets nothing | 403 `wire_denied` |
| 3d | unwired agent can't even list names | 403 |
| 5a | ambiguity at WIRE creation | 409 `ambiguous_credential` ("both … supply K1") |
| 5b | non-conflicting wire allowed | 204 |
| 5c | ambiguity at PUT | 409 `ambiguous_credential` ("v1 already supplies it") |
| 6 | auth mode env + source | `{"authenticated":true,"mode":"env","source":"v1"}` — vault NAME, never the value |
| 7a | rotated (pre-restart) token on secret path | 401 |
| 7b | stopped agent's token stays dead | 401 |

The third ambiguity door (**spawn**, `env_for_agent` bails before exporting) could not be forced live —
every wire-creation path funnels through `add_wire`'s check, and the only way to reach an ambiguous
board is a raw sqlite insert / restored export (no `sqlite3` in the image). It is covered by the engine
unit test `env_for_agent_refuses_rather_than_choosing_a_winner` and by inspection (`env_for_agent` calls
`find_ambiguity(agent,None)` and `bail!`s). So all three doors hold: wire + PUT live, spawn by test+read.

## Design review notes (strong)
- `offered_keys = declared ∪ stored`, so ambiguity cannot be dodged by leaving a key undeclared or by
  storing an undeclared key. AAD binds ciphertext to `node/key` (relabel/paste fails to decrypt — unit
  test `ciphertext_is_bound_to_its_vault_and_key`). Vault PUT/GET-names/DELETE live under the
  **engine-secret** realm (host-only), so even the operator UI has no route that returns a value;
  `secret_get` is a separate **node-token** realm. `WHEEL_TOKEN` is a 0600 file, prompt is a file,
  vault env is delivered via `cmd.env` — nothing sensitive on argv (contract §5b).

## Two by-design residuals — on record, NOT new findings
1. **`vault::redact` is accidental-echo protection, not a boundary** (the code says so, l.328-333). It skips
   values < 8 chars and only does literal substring replace, so a hostile agent that transforms a secret
   (base64/split/reverse) before printing, or holds a <8-char secret, defeats it. This is expected: the
   agent is untrusted code *holding the value in its own env* — "logs/transcript/WS never carry the value"
   is true for the ENGINE's own emission (verified: board/auth/BoardChanged payloads carry no value) but is
   not, and cannot be, enforced against the agent itself. Same class as F008 (accepted).
2. **Docker/M1 single-uid: sibling `/proc/<pid>/environ` is readable.** In the docker backend all children
   share one uid, so agent B could read agent A's vault env from `/proc`. This is the documented M1 KNOWN
   GAP (isolation boundary = project until per-node uid lands, M2/M3). I verified the **process** backend
   resists this cross-uid (F007 review); docker/M1 does not, by design. No new gap.
