# §0b adversarial review — docs/plans/api.md + crates/wheel-api (M0/M1)

Reviewer: ADVERSARY. Verdict: **strong plan, strong implementation — approved.** I went in on my TB1/TB2
attack list and most of it is already closed in code, correctly. Below: what I verified holds (so I don't
re-probe it blindly later), a small set of genuine must-verify / must-test items, and one cross-boundary
invariant that is the engine's job, not the API's. No manufactured findings.

## Verified-in-code, credited (I will not re-probe these without a reason)
- **Fail-closed tenancy:** `ProjectScope` is the only way to name a project in a handler and it has already
  verified the JWT + asserted `owner_id == sub`. No raw `:id` → project row path. This is the right shape.
- **404-indistinguishability:** unowned and nonexistent both 404; ingress uses a deliberately-named
  `load_unauthenticated_for_ingress`. Enumeration oracle closed.
- **Header hygiene (hop.rs):** strips hop-by-hop AND `Connection`-nominated headers, scrubs the client's
  `x-auth-token`/`authorization`/`x-project-id`/`host` from upstream, and drops `x-wheel-*` on ingress so a
  public caller can't forge our trust markers. Client creds never reach the engine; the host bearer is
  attached by us.
- **Traversal:** BOTH proxy.rs (line 29) and ingress.rs (line 32) reject a `..` segment before building the
  upstream URL — not left to the upstream to normalise.
- **Body cap:** both the authenticated proxy and ingress cap the buffered body at `ingress_body_limit_bytes`
  (5 MiB default) → `413`. The unbounded-stream memory-exhaustion vector is closed on both routes.
- **CORS:** `AllowOrigin::list` (allowlist, not reflection) from `CORS_ALLOWED_ORIGINS`.
- **Config interlock:** a dev HS256 token while `WHEEL_ENV != dev` refuses to boot (tested).
- **Auth negative suite:** alg=none, RS256↔HS256 confusion, unknown-kid JWKS throttle, header precedence —
  26 tests, incl. the four I named. This is exactly the TB1 matrix.
- **sqlx bind, not interpolate:** injection-proof without a compile-time DB dependency — correct call.

## Must-VERIFY when the stack boots (unconfirmed; not yet claims)
1. **Encoded-slash / double-decode traversal** (my probe, low confidence): the `..`-segment check runs on
   axum's already-decoded `rest`, but the chain re-decodes API→host→engine. Test `%2f`, `%252e..`, overlong
   UTF-8, mixed, and a leading-slash `rest`, to be sure none reconstruct a `..` segment or an encoded slash
   the host/engine re-decodes to escape `/engine/` toward `/ingress` or `/v1/cli`. `redteam/pocs/proxy-ingress/`.
2. **ws-ticket enforcement:** verify the events WS actually requires the single-use, 30s, (user,project)-bound
   ticket — no replay, no cross-project reuse, expiry honoured — since browsers can't set the auth header on
   the handshake and the JWT must never be in the URL.
3. **CORS in prod:** confirm `CORS_ALLOWED_ORIGINS` is the wheel.dev/Vercel set only (never `*`, never
   credentialed reflection). Config-time check.
4. **Rate-limit boundary burst:** API already documents the fixed-window 2× boundary burst as accepted — I
   agree it's acceptable v1; noting it so it isn't "found" later as if new.

## Cross-boundary invariant → SDK (NOT an API bug)
The authenticated proxy forwards ANY `rest`, including `v1/cli/*`, to the engine with the **host/engine
bearer**. So the ENGINE must discriminate token types: `/v1/cli/*` requires a per-NODE token and must reject
the host/engine bearer; the control-plane routes must reject node tokens. The API is correct to forward; the
defence is engine-side. Cross-ref finding 002 (#2) / 005 (one shared authz fn). Flagged to SDK.

## Process backend (M3)
api.md asks for my design review before the `process` `Sandbox` backend is built — that is finding 003's
per-node-uid model (PM ruling: per-node uid, ambient CAP_SETUID/SETGID only, WHEEL_TOKEN via 0600 file). I'll
review the backend design when API drafts it, before code.
