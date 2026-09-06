# 028 — A declared-but-empty vault credential reports `authenticated:true mode:env`, masks needs_auth, and falsely blocks a real credential

- **Severity:** Medium (fail-OPEN on the auth-status/needs_auth gate + an availability foot-gun). **NOT
  agent-reachable** (declaring vault keys is engine-secret/operator realm), and it leaks nothing — so it is a
  correctness/fail-open, not an escalation. PM traced it from the outside; this pins the scope. Owner: **SDK/Engine**
  (`vault.rs` `offered_keys`/`credential_source`/`find_ambiguity` vs `list_keys`/`env_for_agent`).
- **Status:** CONFIRMED LIVE on a throwaway project — `wheel-engine:dev` image 00:55Z, HEAD 2a50695. PoC:
  `redteam/pocs/vault/run_declared_empty.sh`. Container removed.
- **Boundary:** TB5/TB6 (credential subsystem / auth reporting).

## Root cause — two disagreeing notions of "credential present"
- **Presence for REPORTING and AMBIGUITY** uses `offered_keys = declared ∪ stored` (a vault's `config.keys`
  UNION what is actually stored). `credential_source` / the `GET auth` `mode:"env"` decision / `find_ambiguity`
  all run on this.
- **Presence for DELIVERY** uses `list_keys` = STORED values only. `env_for_agent` (child env) and
  `wheel secret get` run on this.
A key that is DECLARED (`config.keys`) but never `PUT` exists in the first set and not the second, so the two
disagree.

## What a declared-but-empty `CLAUDE_CODE_OAUTH_TOKEN` does (all confirmed live)
1. **Reports authenticated on declaration alone** — `GET /v1/agents/:id/auth` →
   `{"authenticated":true,"mode":"env","source":"v"}` with NO value ever stored. (PM's traced S1.)
2. **Masks needs_auth** — the agent starts (`status: starting`) rather than surfacing `needs_auth`. An operator
   or UI relying on `GET auth` believes the agent is env-authenticated when it is not; a `run_on_startup` agent
   starts and will fail on its first real API call instead of prompting for auth.
3. **No credential in the child env** — read as the child's own uid, the child environ has NO
   `CLAUDE_CODE_OAUTH_TOKEN`/`ANTHROPIC_*` (delivery is stored-only). So it reports authed but runs with nothing.
4. **No phantom value** — `wheel secret get v/CLAUDE_CODE_OAUTH_TOKEN` → `not_found: … is not set`. Good: there
   is no value to leak; this is not a disclosure.
5. **Availability foot-gun** — the empty declaration participates in `find_ambiguity`, so it FALSELY BLOCKS
   wiring a second vault that actually holds the credential:
   ```
   v  declares CLAUDE_CODE_OAUTH_TOKEN (no value);  v2 has a REAL value.
   wire a->v  ok ; wire a->v2 -> 409 ambiguous_credential "both v and v2 supply it"
   ```
   An operator who declares a key intending to fill it later cannot then wire the vault that genuinely supplies
   it — the empty declaration wins the ambiguity it has no business participating in.

## Not affected
Not agent-reachable (an agent cannot declare vault keys — node create/PATCH is engine-secret realm). No secret
is exposed (there is none). The redaction/board/secret-get paths are all correct. This is purely the
declared-vs-stored inconsistency surfacing as a wrong "authenticated" answer and a false ambiguity.

## Fix (pins the scope for SDK)
Base credential PRESENCE on an actual STORED value, not on a declaration:
- `credential_source` / the `GET auth` `authenticated`+`mode:env` decision / the needs_auth gate: a credential
  key counts as present only if `list_keys` (or a real `get`) has a value for it. A declared-but-empty key →
  `authenticated:false` / `needs_auth`, so the state is honest.
- `find_ambiguity`: a key should "supply" a credential for ambiguity only once it has a stored value (or, if
  declaration-time ambiguity is deliberately wanted as an early warning, it must not BLOCK — warn, don't 409 —
  and it must not also drive the authed decision). Keeping declared-key ambiguity as a create-time *warning*
  while presence is stored-based resolves both #1-#2 and #5 without losing the early-signal intent.
Declared keys are fine for UI hints ("this vault will supply X"); they must not decide "is this agent
authenticated" or block a real credential.

## RE-VERIFY (2026-09-06, fresh image 05:37Z from d206b95 / engine 58a333c) — Face 1 FIXED
- **Face 1 (the S1) FIXED**: `GET /v1/agents/:id/auth` for a declared-but-empty `CLAUDE_CODE_OAUTH_TOKEN`
  now returns `{"authenticated":false,"mode":null}` (was `authenticated:true mode:env`). `credential_detail`
  now reads `list_keys` (STORED only), with a comment citing this finding's rationale. Faces 3-4 unchanged-
  correct (secret get → not_found; child env carries no credential).
- **Face 5 (PM overruled SDK 409→warning) — STILL 409, acceptance test RED**: an empty declaration in vault
  `v` still 409-blocks wiring a second vault `v2` that holds the REAL value ("ambiguous credential … both v
  and v2 supply it"). SDK deliberately KEPT declared-key ambiguity (test `a_declared_key_is_still_enough_to_
  be_ambiguous`, `find_ambiguity` still uses `offered_keys`). PM has overruled this to warning-not-409; this
  PoC (`run_declared_empty.sh` §5) is the acceptance test and is currently RED — the change is not yet
  implemented. Hand to SDK: `find_ambiguity`/the wire-create path must WARN, not 409, on a declared-but-
  unfilled key (presence is stored-based per Face 1; ambiguity should be too, or at least non-blocking).
- **"no longer starts a child" — NOT isolable in my offline container (control-proven)**: I nearly reported
  the declared-empty agent as still spawning a child (it sits in `starting` with a live `claude` child). A
  control — an agent with NO vault wire at all — behaves IDENTICALLY (`starting` + child, 13s). So
  "starting-with-a-child" is generic behaviour for ANY unauthenticated agent in an offline test container (no
  reachable Anthropic auth), NOT a declared-empty bug. The load-bearing fix (`authenticated:false`) is
  confirmed; the child-spawn nuance can't be judged offline and I make no claim on it.
