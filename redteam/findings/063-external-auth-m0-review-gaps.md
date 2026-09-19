# 063 — External auth (M0, #136): four gaps the threat-model table does not cover, each run-verified

- **Severity:** Medium overall; individual items below (A Medium — High for hosted multi-tenant, B Low-Medium,
  C Low, D Low-Medium, E Low). None is a bypass of the controls #136 sets out to build: the algorithm-from-key,
  mandatory `aud`, `(issuer, subject)` mapping, no-`wht_`-minting and proxy-peer controls all hold, and QA's nine
  mutants all die (see "What holds"). These are the places the design's own table (§6) is silent.
- **Owner:** SDK/API (`crates/wheel-api/src/auth/{jwks,external}.rs`, `http/{hop,actor}.rs`).
- **Status:** OPEN. Filed from the pre-merge review of #136 (head `d54b56c`); every PoC below is a throwaway test
  against that commit, reproduced 2026-09-19.
- **Method:** source read of the whole diff, then PoC tests run against the real verifier, plus mutation testing of
  the controls. Where a mutant "survived" I re-ran it with a private `CARGO_TARGET_DIR` (see the note on the shared
  target dir at the end) before believing it.

## A. The JWKS cache never expires a key, so a key the IdP removes keeps verifying (Medium; High for hosted use)
`JwksCache::key_for` returns a cached `kid` from the read-locked fast path and refreshes **only** on an unknown-`kid`
miss (throttled to once a minute). Nothing ever ages an entry out, so the refresh that would drop a removed key never
runs unless some request happens to present a `kid` we do not hold. An attacker holding the retired key presents only
that `kid`, so it never triggers the refresh that would remove it.

*PoC (run):* serve a JWKS with the RSA key → verify a token OK (1 fetch) → the server swaps its body to a set without
that key → a token minted **after** the revocation with the retired key verifies as `mallory-with-stolen-key`, and the
server has still seen exactly **1** fetch. Output: `AFTER REVOCATION: verify=Ok("mallory-with-stolen-key") fetches=1`.

Removing a compromised key from the JWKS is the standard IdP response to a key compromise, and this is the credential
path for every `external`/`jwks` deployment including AgentGrid's. The design table (§6 #7) covers refetch
*amplification* (the throttle) but not this inverse. The pre-existing Clerk-only cache has the same shape; #136 makes it
the general path. **Fix:** give cache entries a maximum age (and honour `Cache-Control: max-age` on the JWKS response);
refresh in the background or serve-stale-while-revalidate so a refresh never stalls a request; keep the once-a-minute
throttle for unknown-`kid` misses. Document the revocation latency next to the `WHEEL_EXTERNAL_MAX_TTL_SECS` note.

## B. `proxy_header` takes the *first* value of a duplicated subject header (Low-Medium)
`verify_proxy` reads `headers.get(subject_header)`, which returns the first value. A proxy that **appends** its
assertion (instead of overwriting) leaves the client's own header in front, and the client wins.

*PoC (run):* headers `[x-auth-user: mallory-client-supplied, x-auth-user: alice-proxy-appended]` →
`Ok("mallory-client-supplied")`. Whether real proxies append depends on configuration (nginx `proxy_set_header`
replaces; several others add), which is exactly why this should not be left to luck. **Fix:** require exactly one value
(`headers.get_all(name).iter().count() == 1`), and fail closed otherwise — ambiguity is either an attack or a broken proxy.
The same applies to the email header.

## C. First-login race: concurrent first requests for a new subject → 409 + an orphan account (Low)
`principal_for` does `lookup` → `create_external_user` → `link`. Two concurrent first requests both see no link, both
create an account, and the loser's `link` hits the `UNIQUE (issuer, subject)` and returns `409 Conflict`.

*PoC (run):* `a=Err(Conflict("that external subject is already linked to an account")) b=Ok(<uuid>)`. A browser that
fires parallel requests immediately after first sign-in will see intermittent 409s, and each loser leaves a stray
`users` row. **Fix:** on the unique violation, re-`lookup` and return the winner; do create+link in one transaction.

## D. The custom credential header is on no never-relay list (Low-Medium)
`WHEEL_EXTERNAL_TOKEN_HEADER` (the documented `cf-access-jwt-assertion` case) names a header that *is* the credential,
but it is in neither `hop::CLIENT_ONLY` nor `proxy_asserted_headers()` (which covers only the `proxy_header` pair). The
API therefore relays it to the engine on the authenticated proxy and on public ingress, and the engine's ingress
`envelope_payload` redacts only five header names (`authorization`, the Telegram secret, `x-wheel-secret`, `cookie`,
`proxy-authorization`) — everything else lands in the **agent-visible, stored** message body. A signed IdP JWT (Cloudflare
Access tokens default to 24 h) can thus end up in a transcript, which a guest can read (finding 062). **Fix:** add
`token_header` to the never-relay names in every verifier mode, with a hop test alongside
`a_configured_proxy_assertion_never_crosses_the_hop`.

## E. Controls with no regression pin (Low)
Mutation testing of the verifier (private target dir, see below) — controls whose removal no test notices:
| control removed | result (isolated target dir) | consequence |
|---|---|---|
| `iss` dropped from `required_spec_claims` | **SURVIVES the whole `external_auth` suite** | jsonwebtoken validates `iss` only when present (vendored 9.3.1 doc: "Adding `iss` to `required_spec_claims` will make it required"). PoC with the mutant applied: a validly signed token with **no `iss`** verifies as `Ok("alice")`; on the shipped code it is refused (`a required claim is absent`). The module comment claims this control; no test pins it, unlike `aud`. |
| the `azp` allowlist check removed | **SURVIVES** `external_auth`, `external_identities`, `principal_mapping` | `WHEEL_EXTERNAL_AZP` (which client apps' tokens are accepted) has no behavioural test. |
| `token_header` exclusivity (fall back to `Authorization` when the named header is absent) | **SURVIVES** every external suite | `token_header` appears in tests only at config parse. The documented "only one door is read" property is untested. |
| `exp` dropped from `required_spec_claims` | dies (`a_token_with_no_exp_or_an_expired_one_is_refused`) | control run, environment sane |
| `validate_aud = false`, `sole_audience`, `subject` validation, `disabled` identity, `Provision::Linked`, `Credential::External` mint refusal, `CLERK_*` alias-conflict refusal, `oct` key admission | all die on named assertions | covered |

Add `a_token_with_no_issuer_is_refused`, an `azp` allow/deny pair, and a `token_header`-is-exclusive test.

## What holds (verified by reading and by running)
Algorithm taken from the JWKS key, header must agree, allowlist, `Validation` pinned; `oct`/EC/non-Ed25519 keys never
admitted; `aud` mandatory (`required_spec_claims`) and exact-match; `(issuer, subject)` mapping, never email;
`Credential::External` cannot mint `wht_` tokens; external accounts use a distinct `EXTERNAL_ONLY` sentinel so no
external user is the operator; `proxy_header` fails closed without the `TrustedPeer` extension; configured proxy header
names are lower-cased so the hop strip matches; empty audience / HS* algorithms / alias conflicts refuse to boot.
QA's nine-mutant harness: all nine die on named assertions.

## Design notes (not defects)
- `audience == issuer origin` is only a boot **warn**, although the same PR calls it "the audience-confusion attack
  with the control pointed the wrong way" and AgentGrid's own desktop tokens carry exactly that `aud`. Consider refusing
  by default with an explicit override; a warning in a boot log is easy to miss.
- `proxy_header` trusts any TCP peer inside `WHEEL_TRUSTED_PROXIES`. On a network where untrusted workloads can reach the
  API from inside that range it is full impersonation of any user, including the operator — i.e. it is exactly as safe as
  network isolation, which is finding 048 (still open). The proposal should say so next to §5.
- Disabling an external identity (the only revocation lever) takes effect on the next HTTP request but does not close that
  user's already-open events WebSocket: the bridge re-checks *membership* only, so it lives until the lifetime cap
  (`WS_MAX_LIFETIME_SECS`, default 3600).
- `WHEEL_EXTERNAL_PROVISION=auto` with an IdP that allows open signup means unbounded account — and therefore project/sandbox
  — creation; only `max_projects_per_user` bounds it. Relevant to finding 060.

## Tooling note: the shared cargo target dir gives WRONG mutation results
Worktrees share one `cargo-target`, and artifacts for `wheel-api v0.1.0` built from different trees can be reused for
one another. Measured here: running QA's harness and mine against the shared dir gave *phantom compile errors*
(`unresolved import wheel_api::auth::external`) that the harness reports as COMPILE-FAIL (5 of its 9 mutants on the
first run), and *false SURVIVED verdicts* (3 of my 6 apparent survivors — `sole-audience`, `disabled-identity`,
`oct-admitted` — die when re-run with a private `CARGO_TARGET_DIR`). Only `iss-required`, `azp-allowlist` and
`token-header-exclusive` survive in both environments, so those are the ones reported above. With a private target dir
and touched sources, all nine of QA's mutants die. `qa/tools/mutation_external_auth.py` should set its own
`CARGO_TARGET_DIR` (and should not treat every cargo error as an inconclusive COMPILE-FAIL without saying so).
