# 028 — GET agent/auth reports authenticated:true for a DECLARED-but-EMPTY credential key (status integrity)

- **Severity:** Medium — **status integrity, NOT disclosure.** Auth falsely reports an agent authenticated,
  masking an unauthenticated agent that then starts and silently fails (the "looks fine, isn't" class the
  contract calls out re: silent-unhosted). No secret is leaked and none is exported to the child. (PM traced
  this today and flagged it S1; scope below bounds the blast radius — PM/SDK to set final severity.)
- **Owner:** SDK/Engine (`crates/wheel-engine/src/vault.rs` `credential_source`/`credential_detail` vs
  `env_for_agent`/`list_keys`; `agent/auth` route).
- **Status:** CONFIRMED LIVE on a throwaway project (image 00:55Z, HEAD 2a50695). PoC:
  `redteam/pocs/vault/run_declared_empty_vault.sh`. NOT wheel-dev. Boundary TB6 (auth surface).

## What (all observed live)
A vault node that DECLARES `CLAUDE_CODE_OAUTH_TOKEN` in `config.keys` but has **no value PUT**:
```
GET /v1/agents/:id/auth  -> {"authenticated":true,"mode":"env","source":"vempty"}   # LIES
GET /v1/vault/:id        -> {"keys":[]}                                             # nothing stored
POST /v1/agents/:id/start -> state.status = "starting"  (NOT needs_auth; the start gate passes)
child env (read as child uid) -> CLAUDE_CODE_OAUTH_TOKEN NOT present                # no credential reaches it
```
So auth reports authenticated + the agent starts, while the child has NO credential and the stored-key
listing is empty.

## Root cause — split source of truth
`credential_source`/`credential_detail` (which feed `agent/auth` and the start/lapsed gate) judge on
`offered_keys` = **declared ∪ stored**, so a mere DECLARATION counts as "authenticated." But `env_for_agent`
and the key listing use `list_keys` = **stored only**. For a declared-but-unstored key the two disagree:
auth says authenticated, the child gets nothing. (Using declared∪stored is CORRECT for the AMBIGUITY check —
declaring a key is a commitment to supply it, worth catching early — but WRONG for "is this agent
authenticated right now," which must reflect a STORED value.)

## Impact (bounded — this is why it's status-integrity, not disclosure)
- **Masks an unauthenticated agent:** the operator/UI sees authenticated:true, so the `needs_auth` prompt
  never surfaces; the agent starts, has no token, and fails on its first real turn — a silent broken-agent,
  the exact "looks hosted, isn't" failure the contract says cost hours on YOKE.
- **NOT a leak:** no secret exists; the child env is clean (verified); the stored listing is empty; it does
  not export an empty env var or unlock save_to_vault/MCP. Blast radius = false status + a silently-stranded
  agent, not credential exposure.

## Fix
`agent/auth` (and the start/lapsed gate's notion of "has a credential") must judge on a STORED value, not on
the declared key list: a credential is present only if `list_keys`/a stored value exists for a recognised key
(`credential_source` should consult stored, not `offered_keys`). Keep the AMBIGUITY check on declared∪stored
(that's correct). Net: "authenticated" iff a value is actually stored/available; a declared-but-empty vault
leaves the agent `needs_auth` so the operator is prompted, instead of a silent failure.

## Verify after fix
Re-run the PoC: declared-but-empty key → `authenticated:false` (needs_auth), agent does NOT falsely report
env-auth; once a value is PUT → authenticated:true and the child receives it.
