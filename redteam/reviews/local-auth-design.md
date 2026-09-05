# Design review — local auth (AUTH_MODE=local) — ADVERSARY

Scope: PM 18:34 design (no Clerk → API mints HS256 session JWTs; `AUTH_MODE=local|jwks`;
users table argon2id; signup/login/logout/me; x-auth-token; Web stores JWT in memory+localStorage).
Grounded on: `crates/wheel-api/src/auth/claims.rs` (current verifier) @ 158500f.

**Verdict: NOT approve-as-is.** The existing RS256/HS256 verifier is well built against classic
alg-confusion (each arm pins `Validation::new(alg)` to one algorithm + the matching key, HS256 is
gated to `is_dev()`). Every risk below is in the NEW local-mode code layered on top. Two are High
(mode-selection collision, logout non-revocation) and one is High-systemic (localStorage+XSS). None
block me.

## R1 (High, API) — mode selects the (alg,key,issuer) tuple at BOOT; the token header never selects it
The current verifier does `match header.alg { RS256 => jwks, HS256 if is_dev => dev_secret }`. Adding a
THIRD acceptable HS256 key (SESSION_SECRET) creates a collision: two HS256 keys (dev_secret, session_secret)
and two issuers become acceptable in the same process.
- **Requirement:** `AUTH_MODE` chooses exactly ONE verification profile at boot:
  - `local`: alg **pinned to HS256**, key = `SESSION_SECRET`, issuer pinned to `PUBLIC_BASE_URL`. The
    RS256/JWKS arm and the `is_dev()` dev-secret arm are **both disabled**.
  - `jwks`: alg pinned to RS256 via JWKS, issuer = clerk_issuer. HS256 refused outright (as today).
- The header `alg` is used only to *reject a mismatch*, never to *pick which secret to try*. Do not try
  dev_secret then session_secret (or vice versa) — learning either must not be sufficient.
- **`AUTH_MODE=local` MUST refuse to boot if `WHEEL_ENV=dev` also enables the dev HS256 bypass** — the two
  HS256 paths must be mutually exclusive, or a token signed with the well-known `dev-only-hs256-secret`
  (iss `https://dev.wheel.local`, which my campaign already uses) verifies alongside real sessions.

## R2 (High, API) — dev/mock tokens must be structurally un-acceptable in local mode
- Boot refuses `AUTH_MODE=local` unless `SESSION_SECRET` is present and ≥32 bytes. **No hardcoded fallback.**
- Issuer pinned to `PUBLIC_BASE_URL`; my dev token (`iss=https://dev.wheel.local`) then fails issuer even
  if a secret ever matched. Prod is never `is_dev()` and never carries `dev_secret`.

## R3 (Medium, API) — no account enumeration via login timing or error text
- Login returns ONE generic error ("invalid email or password") for both unknown-user and wrong-password.
- On the unknown-user path, run an argon2id verify against a fixed dummy PHC hash so response time does not
  reveal account existence (constant-ish work either way).
- Signup's "email already registered" is an inherent oracle; mitigate with signup rate-limiting (R4) and,
  recommended, a generic "check your email" response. If kept as-is, document it as accepted Low.

## R4 (High, API) — rate limit cannot be keyed on client-supplied X-Forwarded-For (Railway)
- On Railway the app sees an XFF the client can prepend. **Never** trust raw leftmost XFF for the limiter key.
  Derive the client IP from a trusted position (rightmost entry after Railway's known proxy count, or the
  platform client-IP header); document the trusted-hop count in API.md.
- Because API runs N replicas, a per-replica in-memory limiter is bypassable by spreading requests. Auth
  endpoints are low-volume → also enforce an **email-scoped failure counter in Postgres** (lockout/backoff)
  as the floor. Key = (trusted_ip, normalized_email).

## R5 (High, API) — logout must revoke server-side; 7-day stateless JWT + client-only logout ≠ logout
A captured token stays valid 7 days after "logout" (and R7 makes theft realistic). Minimum viable:
`users.session_version INT`, embedded as a claim; logout(-all) bumps it; every verify checks claim==row
(one indexed read). Gives real logout + a compromise kill-switch. If punted for M1 it is a **documented
KNOWN GAP**, not silent — but I rate it High given R7.

## R6 (Medium, API) — password hashing + token binding
- argon2id, OWASP floor (m≥19 MiB, t≥2, p≥1), per-user salt (argon2 default), store only the PHC string,
  min length ≥12, reject top-N common passwords, pre-hash-cap very long inputs (DoS), never log passwords.
- Token minted fresh at login only; `iat/nbf` sane (5s leeway is fine); no pre-auth token upgrade.
- Session JWT MUST NEVER appear in a URL/query (downloads/SSE included). WS already uses ws-ticket — good.

## R7 (High-systemic, Web) — localStorage JWT + any board XSS = 7-day account takeover
The board renders agent-influenced strings (node names, ctx markdown, log lines, message bodies, tool
responses). One XSS ⇒ read localStorage ⇒ full takeover until exp. Compensating controls are MANDATORY,
not optional, if the JWT stays in localStorage:
- **No `dangerouslySetInnerHTML`** anywhere agent/user data flows; render as text (React default).
- **ctx markdown** rendered through a sanitizer (hardened renderer + DOMPurify): strip `<script>`, event
  handlers, `javascript:`/`data:` URLs, `<iframe>`, SVG script. This is vector #1 (agent writes ctx → renders
  in the operator's browser).
- **Log lines**: strip/escape ANSI, never interpret HTML/newlines as markup.
- **Strict CSP**: `default-src 'self'`; `script-src 'self'` with nonces (NO `unsafe-inline`/`unsafe-eval`);
  `connect-src` = API origin (+ Clerk if used); `object-src 'none'`; `base-uri 'none'`;
  `frame-ancestors 'none'` (clickjacking on the OAuth device-code submit UI).
- **CORS (API):** allowlist the exact web origin(s); never `*` with credentials/token reflection.
- CSRF: bearer-in-custom-header (x-auth-token) is not cookie-CSRF-exposed — do NOT move the session to a
  cookie without adding SameSite=strict + CSRF tokens.
CSP + universal encoding + markdown sanitizer are what make the accepted localStorage tradeoff survivable.
Without them the tradeoff is not acceptable.

## To verify once built
- A token with `{alg:none}`, RS256-in-local-mode, HS256-signed-with-dev-secret, and wrong-issuer all → 401.
- login timing/error identical for unknown-user vs wrong-password.
- XFF spoof does not reset the limiter; email lockout holds across replicas.
- logout then reuse of the same token → 401.
- CSP present + a ctx-markdown `<img onerror>` / `<script>` payload does not execute; localStorage
  unreadable to injected markup.
