# 016 — Vault campaign results (M1.6): one CRITICAL, the rest resist

- **Scope:** PM's vault/env-credentials campaign against `wheel-engine:dev` @ 39cbcb0 (docker backend), 2026-09-05.
- **Owner:** SDK/Engine. **Status:** headline finding filed as **015 (CRITICAL)**; all other claims RESIST live.
- **PoCs:** `redteam/pocs/vault/{run_env_inheritance,run_env_exploit,run_vault_claims}.sh`.

## Result table
| Claim (PM) | Verdict | Evidence |
|---|---|---|
| **child env isolation** | **CRITICAL — F015** | child inherits WHEEL_VAULT_KEY + WHEEL_ENGINE_SECRET (no env_clear at supervisor/mod.rs:203); live control-plane bypass proven |
| encryption at rest | RESIST | marker plaintext count 0 across `/data/wheel.db`, `-wal`, `-shm`; AES-256-GCM, per-value random nonce, AAD binds ciphertext to node/key (vault.rs) |
| key from WHEEL_VAULT_KEY only | RESIST | `VaultKey::from_base64` is the sole key path; their test `a_different_project_key_cannot_read` proves a wrong key fails to decrypt |
| write-only API — no value ever returned | RESIST | `GET /v1/vault/:id/:key` → 405 (no read route); `/v1/board` carries key NAMES only, never the value |
| ambiguity refused at WIRE creation | RESIST | 2nd vault offering the same key → **409** on `POST /v1/wires` |
| ambiguity refused at PUT | RESIST | `PUT` of a key that would collide across an agent's wired vaults → **409 ambiguous_credential** |
| ambiguity refused at SPAWN | RESIST (source+their test) | `env_for_agent` calls `find_ambiguity(...)?` and `bail!`s before building env; test `env_for_agent_refuses_rather_than_choosing_a_winner` covers the import/legacy-wire path |
| auth mode `env` + source correctness | RESIST | `GET /v1/agents/:id/auth` → `{"authenticated":true,"mode":"env","source":"v1"}` — names the vault supplying the recognised credential key |
| CLI wire-gated read + rotated/deleted token | RESIST (F013) | covered by the CLI-gated campaign: node token dies on restart/stop, unwired/absent target denied, write⇒read only |

## Notes / residual (documented, not new findings)
- **`redact()` is accidental-echo only** (vault.rs:328-347, explicitly disclaimed): it blanks a secret only
  where it appears VERBATIM and only if ≥8 chars. A short secret (<8) or an agent that transforms a value
  before printing is NOT redacted in logs/transcript. This is by design (an agent holds its own env; redaction
  is not a containment boundary) — matches the F008 posture. Flagged so it is not mistaken for a guarantee.
- **transcript log at supervisor/mod.rs:344** writes the exact inbound-envelope stdin bytes WITHOUT redact();
  inbound envelopes are messages TO the agent, so they carry a vault value only if a peer put one there — low
  concern, but worth a redact() pass on that line for parity once F015 is fixed.
- Once F015's `env_clear()` lands, re-run all three PoCs; only `run_vault_claims.sh` should stay green and the
  two env PoCs should go clean.
