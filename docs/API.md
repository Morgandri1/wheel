# `api.wheel.dev` — public API

Stateless gateway in front of `wheel-host`. Owned by the API team. Contract: `ARCHITECTURE.md` §5.

The API never talks to a container runtime and never talks to an engine directly. Every sandbox
operation is an authenticated call to the single `wheel-host` machine over private networking.

## Authentication

Three providers, selected by `AUTH_MODE`. All three end at the same verified user id, and nothing
downstream — the membership check above all — can tell which one ran. Swapping providers is
configuration, not code.

| `AUTH_MODE` | Credential | Verified against |
|---|---|---|
| `local` (built in) | HS256, issued by this API | `SESSION_SECRET`, plus a live row in `sessions` |
| `jwks` | RS256, issued by an external provider | the provider's JWKS |
| `external` | a JWT from the deployer's issuer, or an assertion from a proxy they run | the deployer's JWKS with a **mandatory audience** and a key-pinned algorithm, or the TCP peer. See [External auth](#external-auth-auth_modeexternal). |
| any | `wht_…` API token, issued by this API | its SHA-256 in `api_tokens`, not revoked (see [API tokens](#api-tokens-every-auth_mode)) |

`AUTH_MODE` must be set explicitly in production — an unset value refuses to boot rather than
defaulting, because guessing wrong either rejects every real user or accepts tokens from the wrong
issuer. Under `jwks`, empty `WHEEL_JWKS_URL`/`WHEEL_JWKS_ISSUER` also refuse to boot: a placeholder
that looks like configuration is worse than a missing one, since it starts and then rejects every
token for a reason nobody can see.

A token minted under one mode is rejected under the others. `local` and `jwks` are different
algorithms verified with different keys, so that needs no special case. `external` is pinned to an
issuer that boot refuses to let equal `WHEEL_JWKS_ISSUER` or `PUBLIC_BASE_URL`, so the two
JWKS-backed planes cannot stand in for each other either — see
[Configuration interlocks](#configuration-interlocks).

## Local auth routes (`AUTH_MODE=local`)

All six return `404` when `AUTH_MODE` is not `local`, so switching providers cannot leave a second
way in.

### `POST /v1/auth/signup`
```json
{ "email": "person@example.com", "password": "at least ten chars" }
```
`201` →
```json
{
  "token": "<HS256 session JWT>",
  "expires_at": "2026-09-12T18:40:00+00:00",
  "user": { "id": "<uuid>", "email": "person@example.com", "created_at": "..." }
}
```
Send `token` as `x-auth-token` on every subsequent request.

- Email is stored `citext`, so `Alice@x.com` and `alice@x.com` are one account. Without that,
  address casing silently creates a second user who cannot see the first one's projects.
- Password: 10–1024 **characters**, counted as characters rather than bytes so a ten-character
  passphrase in a non-Latin script is not wrongly rejected. No composition rules — requiring a digit
  and a symbol pushes people toward `Password1!`, which is measurably worse than length alone.
- Hashed with argon2id as a PHC string, so parameters can be raised later without invalidating
  existing rows.
- `409` if the email is taken. Rate limited globally per hour.
- `403`, with the generic `forbidden` body, when signup is closed. See the signup policy below.

### Signup policy (`WHEEL_SIGNUP`)
`closed` (the default: unset or empty) or `open`, for the `wheel-api` binary and `wheeld` alike. Any
other value refuses to boot, and `invite` is refused with a pointer to the route below.

- **Closed:** `POST /v1/auth/signup` answers `403` before it counts against the signup limit, and
  creates nothing.
- **Closed is not inferred from how the box looks.** An earlier version opened it on a loopback-only
  `wheeld`, and a review showed that is still reachable by every account on the machine and by any
  proxy or tunnel that presents `127.0.0.1`. Opening it is a decision made out loud:
  `WHEEL_SIGNUP=open`.
- **Why:** the embedded backend runs every account's agents as the daemon's own user. An open signup
  on a public box is a stranger's code on it.

### `POST /v1/auth/users`
The owner adds an email/password account, whatever the signup policy. Same body as signup. `201` →
`{ "id", "email", "created_at" }`, with no session: the new user signs in themselves.

- **Only the owner may call it:** the token-only account `wheeld` created on first boot, the one
  account a signup can never produce. Anyone else gets `403`, including people the owner added and
  their tokens, so a closed signup cannot be reopened from the inside.
- `401` without a credential; `409` if the email is taken; `400` for a bad email or password.
- `404` when `AUTH_MODE` is not `local`.

### `POST /v1/auth/login`
Same body. `200` with the same shape as signup.

**`401` for every failure** — unknown email, wrong password, malformed input — with one identical
body. Anything else confirms which addresses are registered.

Unknown emails are verified against a real argon2 hash before failing. Skipping the hash would make
login measurably faster for addresses that are not registered, turning the endpoint into an
account-existence oracle that timing alone would reveal.

Rate limited **per email**, 10 attempts per 15 minutes, counted in Postgres so the limit holds
across replicas rather than multiplying by the replica count. Per-email rather than per-IP because
the attack an IP limit misses is a password spray from many addresses against one account. Over the
limit returns `429`, not `401`: pretending the password was wrong would hide the lockout from a
legitimate user whose account someone else is attacking.

### `POST /v1/auth/logout`
`204`. Deletes the session row, so the token stops working immediately.

Succeeds even with an expired, already-revoked, or absent token — logging out should never fail.

This is why sessions are rows rather than pure stateless JWTs: a stateless token cannot be revoked
before it expires, and a "logout" that leaves the token working for seven more days is not a logout.

### `GET /v1/auth/me`
`200` → `{ "id", "email", "created_at", "owner" }`. `owner` is true for the token-only owner, the
one account that may call `POST /v1/auth/users`. `401` if the account no longer exists — the
signature can still verify over a user that has been deleted.

### `POST /v1/auth/password`
```json
{ "current_password": "...", "new_password": "..." }
```
`204`. Requires the current password even though the caller is already authenticated: that is what
stops a stolen session token from becoming a permanent takeover, since the attacker still cannot
lock the owner out.

**Revokes every session, including the caller's own.** If the password was changed because it was
compromised, leaving existing sessions alive would defeat the point. Clients must log in again.

Email-based password *reset* is M3 — it needs a mail provider, and there is none yet.

## External auth (`AUTH_MODE=external`)

The deployer brings an identity system and Wheel verifies and maps it. Full design, threat model and
the reasoning behind every refusal below: `docs/proposals/external-auth.md`.

`external` is a stricter superset of `jwks`. It adds a **mandatory audience**, an explicit algorithm
allowlist, **key-pinned algorithm selection**, and local principal mapping. `jwks` keeps its missing
audience validation because that is the deployed contract; `external` is where the rigour lives.

**There is no account unification.** A foreign subject is never a Wheel principal. Every verified
subject is mapped to a Wheel `users.id` that Wheel minted, and that is what `projects.owner_id`,
`project_members.user_id` and `on_behalf_of` hold.

### Two verifiers

`WHEEL_EXTERNAL_VERIFIER` picks one, and there is no default.

| Verifier | What is verified | Where the credential is read from |
|---|---|---|
| `jwks` | the signature, `iss`, `aud`, `exp`, `nbf`, and optionally `azp` and a lifetime cap | `x-auth-token` / `Authorization: Bearer`, or `WHEEL_EXTERNAL_TOKEN_HEADER` (e.g. `cf-access-jwt-assertion`) |
| `proxy_header` | **nothing about the assertion.** The proxy is the verifier; the control is that the request arrived *from* the proxy | `WHEEL_EXTERNAL_PROXY_SUBJECT_HEADER` |

A deployment configured for one refuses the other's credential rather than falling through to a
weaker check: a bearer token under `proxy_header` is a 401, and a subject header under `jwks` is
never read.

### The algorithm comes from the key, not from the token

This is the structural change, not an addition to a list. `jwks` mode branches on `header.alg` to
pick a verifier, which hands the choice of verifier to whoever wrote the header. Under `external`
the order inverts:

1. `decode_header` → `kid`. **A token with no `kid` is refused.**
2. The `kid` resolves in the key set to a key *and the algorithm the JWK's own key material
   declares* — `RSA` → `RS256`, `OKP` with `crv: Ed25519` → `EdDSA`. Never the token's `alg`.
3. Refused unless `header.alg` **equals** the key's algorithm.
4. Refused unless the key's algorithm is in `WHEEL_EXTERNAL_ALGS`.
5. Decoded with a `Validation` pinned to that **one** algorithm, never a list.

The JWKS loader additionally refuses to hold an `oct` (symmetric) key at all, and refuses an `OKP`
key on any curve but Ed25519 — X25519 is a key-*agreement* key, and importing one as a signature key
is the shape of a downgrade, not a mistake. `alg: none` has no algorithm variant, so it fails at
`decode_header`; that is a property of a dependency, so it has its own test rather than a comment.

`WHEEL_EXTERNAL_ALGS` refuses a symmetric algorithm **at boot**, by name. A verification key that is
also a minting key means anyone who can verify can forge.

### Audience is mandatory, exact, and Wheel-dedicated

`aud` is **required**: a token that does not carry one is refused. That needs saying because it is
not what the library does on its own — with `validate_aud = true` and an audience configured,
`jsonwebtoken` passes a token whose `aud` is *absent*, since validation is only applied to a claim
that is present. Wheel names `aud` in `required_spec_claims` explicitly, and a test pins it.

Comparison is **exact string equality** against the configured set. Never a prefix: `wheel:` as a
prefix would accept `wheel:some-other-deployment`, which is the cross-tenant confusion the check
exists to stop. `iss` and `exp` are required for the same reason — an absent issuer would otherwise
skip the pin.

**Choose an audience that means *this Wheel deployment* and nothing else.** The obvious value — the
issuer's own origin — is the one that must not be used. An issuer that serves several of its own
surfaces typically mints for all of them under one issuer, and several of those already carry the
issuer origin as their `aud`. Configure that here and every one of them becomes a valid Wheel
credential.

```
WHEEL_EXTERNAL_ISSUER=https://accounts.example.com      # what the IdP puts in `iss`
WHEEL_EXTERNAL_AUDIENCE=https://api.wheel.example       # what WE are, and nothing else
```

`wheel`, `wheel:prod`, or the deployment's own API origin are all fine; the issuer origin is not.
Wheel **refuses to boot** when a configured audience equals the pinned issuer or its origin.
It cannot know what else an issuer serves, so there is one explicit override,
`WHEEL_EXTERNAL_ALLOW_ISSUER_AUDIENCE=1`, for an issuer that mints tokens for Wheel and nothing else.

On a multi-valued `aud`, any-match is the default: a token carrying
`["https://wheel.example/api", "https://wheel.example/userinfo"]` is what several IdPs emit as a
matter of course, and a control nobody can enable is not a control. What that admits is narrow and
named: **another relying party listed in the same token can replay that token here.** The IdP named
that party deliberately. `WHEEL_EXTERNAL_SOLE_AUDIENCE=1` is the lever for deployers who do not
extend that trust.

### Time, and the lifetime cap

`exp` is required, `nbf` is validated when present, and leeway is 5 s. `WHEEL_EXTERNAL_MAX_TTL_SECS`,
when set, *additionally* requires `iat` and refuses `exp - iat` above the cap — a token with no `iat`
under a configured cap is refused, because a cap that cannot be computed is not a cap. It is
optional because an 8-hour access token is a legitimate thing for an IdP to issue, and a cap that
breaks every real deployment gets set to infinity by the first person who hits it. **No cap means
revocation latency equals token lifetime.**

### Principal mapping

New table `external_identities` (migration `0007`, both dialects), keyed `UNIQUE (issuer, subject)`:

| column | |
|---|---|
| `provider` | the operator's label, for display and logs |
| `issuer` | the `iss` that was actually verified |
| `subject` | the claim named by `WHEEL_EXTERNAL_SUBJECT_CLAIM` (default `sub`) |
| `user_id` | the Wheel `users.id` this maps to |
| `email` | **display only, never a link key** |
| `created_at`, `last_seen_at`, `disabled_at` | |

The key is `(issuer, subject)` and **not** `(provider, subject)`. `provider` is a label an operator
may retitle; `issuer` is what was cryptographically asserted. Keying on the label would mean that
pointing `provider=acme` at a different issuer silently merges two populations into one set of
accounts. Keying on the issuer makes the same typo fail closed, into new empty accounts.

**Never auto-link by email.** If a token carries `email` it is stored for display and never used to
find an existing account. An IdP that lets a user set an unverified address would otherwise be a
one-step takeover of any local Wheel account whose address an attacker can guess. The only automatic
link is `(issuer, subject)`.

`WHEEL_EXTERNAL_PROVISION` has no default and must be stated:

- **`auto`** — a verified token for an unknown `(issuer, subject)` creates a Wheel user and links it.
  Correct when the IdP's population *is* the intended Wheel population. **If your issuer lets anyone
  sign up, `auto` lets anyone into Wheel.**
- **`linked`** — a verified token for an unknown `(issuer, subject)` is rejected until an operator
  links it. There is no bootstrap problem: the `wht_` operator token from first boot is the
  pre-existing principal that performs the first link.

Three cases an operator should know about before choosing an IdP:

- **A subject disappears** (deleted upstream). **Wheel does not find out.** There is no back-channel
  logout and no SCIM in v1; access ends only because the IdP stops issuing tokens. The operator's
  lever is `DELETE /v1/auth/external-identities/{id}`.
- **A subject is reused** — the IdP re-assigns a retired `sub` to a different human. The new human
  **inherits the old one's Wheel account, projects and memberships, and Wheel cannot detect it.**
  OIDC Core §2 requires `sub` never to be reassigned, so this is a provider defect — but it is a
  requirement *of the deployer's IdP*, stated here because Wheel cannot enforce it. Where the IdP
  has a better immutable identifier (`oid` on Entra, `user_id` on several others), point
  `WHEEL_EXTERNAL_SUBJECT_CLAIM` at it. `last_seen_at` makes a dormant-then-active identity visible
  to an operator who looks. Residual, not closed.
- **The issuer changes** (an IdP migration, or a hostname move). A new `(issuer, subject)` is a
  **new principal with no projects.** That is deliberate and fail-closed: inheriting an account
  because a URL changed is account takeover triggered by a config edit. The migration path is
  explicit re-linking through the admin route below.

### An external credential may not mint a `wht_` token

`POST /v1/auth/tokens` returns **403** for a caller authenticated by `external`. This is the control
that keeps the whole external lifetime story from being decorative: a deployer's token is
short-lived and revocable by their IdP, and a `wht_` token is neither, so allowing the trade would
let anyone with five minutes of access buy an indefinite credential the deployer's identity system
can no longer take away.

### `proxy_header` — the dangerous mode, and what contains it

If Wheel believes a header, then **anything that can reach Wheel directly can be anyone.** Wheel must
be reachable *only* through the authenticating proxy. A trusted-peer list is the last check, not the
only one. What is enforced mechanically:

1. **Boot refuses** `proxy_header` when `WHEEL_TRUSTED_PROXIES` is empty, naming the missing
   variable. Believing a header from everyone is not a configuration, it is an open door.
2. **Per request, the TCP peer** must be inside `WHEEL_TRUSTED_PROXIES` or the request is 401
   whatever its headers say. Not `X-Forwarded-For` — the peer. The marker is a server-side request
   extension a client cannot forge, and if the middleware that computes it is absent the marker is
   absent and every request fails closed.
3. **The subject and email headers never cross the proxy hop.** They are the credential, so they
   join `x-auth-token` on the never-relay list — on the authenticated engine proxy *and* on public
   ingress, where the proxy attaches them to every forwarded request even though ingress
   authenticates nobody. Relaying one would put "who the edge says is calling" in front of an agent,
   which could replay it back at the API as its author. They cannot be a `const` list, because the
   deployer names them, so they are resolved from configuration at the one function every outbound
   request goes through.
4. **Cross-origin requests are refused 403.** The credential is *ambient* — the proxy attaches it —
   so a hostile page can make a browser issue an authenticated request. JSON routes are covered by
   preflight, but the engine proxy is `ANY` with arbitrary content types, so preflight is luck
   rather than a control. An `Origin` header not in `CORS_ALLOWED_ORIGINS` is refused, **before** the
   identity is resolved, so a refused request provisions nobody and touches no row. A request with
   no `Origin` is not something a page can cause and passes. With the default empty allowlist, no
   browser page may call the API cross-origin at all — which is the correct answer for an ambient
   credential. The allowlist reaches the extractor as a request extension whose **absence** is read
   as an empty list, so a router assembled without the layer refuses every cross-origin request
   rather than admitting every one.
5. **Boot warns, loudly and once**, naming the mode, so the reduced posture is in the first screen
   of logs rather than discoverable only by reading configuration.

`wht_` API tokens keep working under `proxy_header`: they are *this API's own* credential, and the
extractor checks for one before the proxy-header branch. Without that, `wheeld token` — the one
credential an operator can use from a script — would be useless on a proxy-authenticated deployment.

### External identity administration

Three operator-only routes. They **404 unless `AUTH_MODE=external`**, so a deployment that does not
use external auth has no such surface at all. Authorisation is the token-only owner account — the one
`wheeld` creates on first boot, which no signup can produce — the same guard as `POST /v1/auth/users`.

#### `GET /v1/auth/external-identities`
`200` → every link, oldest first. None of it is a credential.
```json
[ { "id": "<uuid>", "provider": "acme", "issuer": "https://accounts.example.com",
    "subject": "auth0|abc", "user_id": "<uuid>", "email": "a@example.com",
    "created_at": "…", "last_seen_at": "…" | null, "disabled_at": null } ]
```

#### `POST /v1/auth/external-identities`
```json
{ "subject": "auth0|abc", "user_id": "<uuid>", "email": "a@example.com" }
```
`201` → the created link.

- **The issuer is not a parameter.** It is taken from configuration, because it is the thing that was
  or will be cryptographically asserted. Letting a caller name one would let an operator link a
  subject under an issuer this deployment never verifies — a row that can never match and looks like
  it should.
- `subject` is validated as a principal (bounded length, no control characters) → `400`.
- The account must exist → `404`. Linking to an absent one would create a row that authenticates
  nobody, silently.
- A subject already linked → `409`.

#### `DELETE /v1/auth/external-identities/{id}`
`204`, `404` for an unknown id. **Soft**: it sets `disabled_at` rather than deleting, so the link is
still visible to an operator afterwards and re-enabling is a decision rather than a fresh provision
under a new Wheel account, which would silently orphan the old account's projects. Verification then
still succeeds — the IdP still vouches for them — and access does not, which is the only revocation
lever Wheel has over a provider with no back-channel logout.

## API tokens (every `AUTH_MODE`)

API tokens are how a client without a browser signs in: a script, CI, or a desktop app such as AgentGrid. They work the
same way against a local `wheeld` and the cloud API. Design and threat model: `docs/proposals/headless-first.md`.

- **Format.** `wht_` followed by 43 base64url characters, which encode 32 random bytes.
- **Sending one.** Use it wherever a session goes: `x-auth-token: wht_…` or `Authorization: Bearer wht_…`.
- **Subject.** A token speaks for the account that minted it. Under `local` that is the user's id; under `jwks` it is the
  identity provider's `sub`; under `external` it is the Wheel `users.id` the subject maps to. Projects and membership
  checks cannot tell a token from a session.
- **Not mintable by an external credential.** `POST /v1/auth/tokens` is **403** under `AUTH_MODE=external`. See
  [An external credential may not mint a `wht_` token](#an-external-credential-may-not-mint-a-wht_-token). A `wht_`
  token presented *to* a proxy-header deployment still authenticates normally — it is this API's own credential, and it
  is checked before the proxy-header branch.
- **Storage.** The server keeps only the token's SHA-256, which is also the lookup key. The value is returned once, when
  it is minted, and never again: not by any route, and not in any log.
- **Failure.** An unknown or revoked token gets the same `401`, with the same body, as every other authentication failure.
  Nothing tells a caller that the token existed or was revoked.
- **Last use.** Every authenticated request stamps `last_used_at`. The revocation check and the stamp are one statement.
- **A dead family stays dead.** A token authenticates only while every token in its minting chain is
  unrevoked. That covers a child minted in the instant before its parent's revocation landed, which the
  revocation itself could not see.
- **A password change ends what that account's sessions minted,** and everything those tokens minted,
  along with the sessions themselves. Tokens the operator minted from the data directory
  (`wheeld token create`, the first-boot operator token) are not a session's, and survive it.

### `POST /v1/auth/tokens`
Authenticated by a session or by an existing token. **`403` for an externally-authenticated caller**
(`AUTH_MODE=external`), so a short-lived credential the deployer's IdP can revoke cannot be traded
for an indefinite one it cannot.
```json
{ "name": "laptop" }
```
`201` →
```json
{ "id": "<uuid>", "name": "laptop", "token": "wht_…", "created_at": "2026-09-11T10:00:00+00:00" }
```
- `name` is 1–64 characters after trimming, with no control characters. Otherwise `400`.
- Minting is rate limited to **20 per account per hour**, counted in the database like the login limit. Over the limit
  returns `429`.
- A token minted *by a token* records its parent as `minted_by`.

### `GET /v1/auth/tokens`
`200` → the caller's own tokens, newest first. It never includes a token value or a hash.
```json
[ { "id": "<uuid>", "name": "laptop", "minted_by": null,
    "created_at": "…", "last_used_at": "…" | null, "revoked_at": "…" | null } ]
```

### `DELETE /v1/auth/tokens/{id}`
`204`. The token stops working on the next request.

- **It revokes the token's whole lineage**: every token it minted, transitively. Whoever used a leaked token to mint
  successors loses them with it, so revoking the leak ends it.
- It is idempotent, and a second revoke keeps the first `revoked_at`.
- It returns `404` for a token that belongs to someone else, exactly as for one that does not exist or an id that is not
  a uuid.

### `wheeld`: the operator token and `wheeld token`
On its first start against a store with no accounts, `wheeld` does three things:
1. It creates `operator@wheeld.invalid`, an account with no password that signs in only with tokens.
2. It writes that account's first token to `<data-dir>/operator-token` (`0600`).
3. It logs the path, never the value.

`wheeld token create [--name N] [--email E] | list | revoke <id>` manages tokens against the local store directly. It
needs no running daemon and no HTTP. Access to the data directory is the boundary, the same one that guards `master.key`.
With no `--email`, `create` mints for the token-only owner.

### Note on revocation vs. `session_version`

The review asked for a `users.session_version` counter. This implements the same guarantee with a
`sessions` table instead: logout and password change delete rows, and every request checks the row
is still live. That is strictly finer-grained — it can revoke one session without ending the others,
which a global version counter cannot — at the cost of one indexed lookup per request. Flagged
rather than silently substituted; say the word if you want the counter instead.


| Header | Required | Notes |
|---|---|---|
| `x-auth-token` | yes | A session JWT (local HS256 or the provider's RS256), or a `wht_` API token. `Authorization: Bearer <token>` is accepted as an alias. |
| `x-project-id` | project-scoped routes | Must be a UUID. If the route also carries the id in its path, **the two must match exactly** or the request is rejected `400`. |
| `x-request-id` | no | Echoed back and attached to every log line for that request. |

Order of operations on every project-scoped request, without exception:

1. Verify the credential. Under `jwks`, the signature against the cached JWKS (RS256 only). Under
   `external`, the signature against a key whose algorithm the *key set* declares, then the
   mandatory `aud`, then everything in [External auth](#external-auth-auth_modeexternal). Under
   `proxy_header` there is no signature, and the check is that the TCP peer is a trusted proxy.
2. Validate `iss`, `exp`, `nbf`, and `azp` when an allowlist is configured.
3. Resolve the principal. Under `external` that is a lookup of `(issuer, subject)` in
   `external_identities`, which yields a Wheel `users.id` — never the foreign subject.
4. Load the project **with membership as part of the query**.
5. Only then run the handler.

Step 3 is a `WHERE` predicate rather than a comparison after the fetch, so "no such project" and
"someone else's project" are literally the same code path. Both return `404`. There is no way to
learn whether an id exists.

### Failure modes

| Condition | Status | Body `error.code` |
|---|---|---|
| No token, malformed token, bad signature, expired, wrong issuer, unknown `kid` | `401` | `unauthorized` |
| Valid token, project not owned by `sub`, or no such project | `404` | `not_found` |
| Token with no `kid`, `alg: none`, a header `alg` disagreeing with the key's, or an algorithm outside `WHEEL_EXTERNAL_ALGS` | `401` | `unauthorized` |
| Missing, wrong, or non-exactly-matching `aud` under `external` | `401` | `unauthorized` |
| Proxy-header assertion from a peer outside `WHEEL_TRUSTED_PROXIES`, or with the trusted-peer layer absent | `401` | `unauthorized` |
| Unknown `(issuer, subject)` under `WHEEL_EXTERNAL_PROVISION=linked`, or a disabled external identity | `401` | `unauthorized` |
| Cross-origin request under `proxy_header` | `403` | `forbidden` |
| `POST /v1/auth/tokens` from an external credential | `403` | `forbidden` |
| An external-identity route without the operator account | `403` | `forbidden` |
| An external-identity route with `AUTH_MODE != external` | `404` | `not_found` |
| `x-project-id` not a UUID, or disagrees with the path | `400` | `bad_request` |
| Ingress on a project with `capabilities.http = false` | `403` | `forbidden` |
| Per-user project cap reached | `409` | `conflict` |
| Body over the cap (5 MiB default) | `413` | `payload_too_large` |
| Ingress over the rate limit | `429` | `rate_limited` |
| Host unreachable | `502` | `bad_gateway` |
| Host too slow (30 s default) | `504` | `gateway_timeout` |

The 401 body is identical for every cause. The specific reason is logged for operators but never
returned, so the response cannot be used as an oracle for which part of a forged token was wrong.

### Error body

Every error, on every route:

```json
{ "error": { "code": "not_found", "message": "The requested resource does not exist." } }
```

## Access tiers

Every project-scoped route requires exactly one tier, and **anything unlisted is refused**. Full
table and reasoning: `docs/proposals/shared-projects.md` §5.

| Tier | What it is |
|---|---|
| **admin** | Everything. Board structure, members and invites, vault, project lifecycle, settings. |
| **prompter** | Manage context, and prompt agents: read the board, write ctx content, send messages, start/stop/restart agents. |
| **guest** | View only. Board, transcripts, logs, events. Sends nothing. |

**The project's creator is always an admin** and cannot be demoted or removed: that is
`projects.owner_id`, not a `project_members` row, so there is only ever one answer to who owns a
project. An admin may make another admin.

This replaces the old `project.owner_id == jwt.sub` check. Non-members still get **404**, never 403 —
the check is a `WHERE` predicate, so "does not exist" and "not yours" stay one code path and cannot
become an enumeration oracle. A member at too low a tier gets **403**, because they have already
proved the project exists to them and an honest answer lets a UI say why.

Notable boundaries, each with a reason:

* **Agent lifecycle is prompter; *project* lifecycle is admin.** A prompter who cannot start an agent
  cannot prompt it, since a message never starts a process. Stopping the *sandbox* destroys other
  members' running work. Consequence: a prompter arriving at a stopped project must ask an admin.
* **The vault is admin only, including listing key names.** Per ADVERSARY 037 a vault value is
  readable by every agent in the project, so sharing a project must not share the creator's
  third-party credentials. Attaching or clearing an agent's LLM credential, and invoking a tool that
  spends one, are admin for the same reason. Enforced server-side: `GET .../board` redacts every
  vault (and any other node with `has_redactable_credentials()`) via `NodeConfig::redact_credentials`
  before the response leaves the engine for any caller below admin (`board_routes.rs::get_board`) —
  a client never receives key names to hide client-side.
* **`/v1/cli/*` is refused to every tier, admins included**, when reached through the proxy. It is the
  node-token realm; the API cannot attribute an actor there, so it declines to carry one.
* **Writing ctx content uses `PUT /v1/nodes/{id}/content`**, not `PATCH /v1/nodes/{id}`. The patch
  route also carries agent config, and a tier may not have powers that depend on a request body.
* **`/p/` public ingress is outside the tier system entirely** — see below. A tier is never a way
  into it, and it is never a way around a tier: a hit arrives as `type=endpoint`, carries no actor,
  and a guest cannot use it to do what `POST .../agents/{id}/send` would have refused them.

Known gap: **a prompter cannot write table rows in v1.** There is no structured control-plane
row-write route for anyone — `POST /v1/tables/{id}/query` is read-only SQL by design — so granting it
means designing one. Named rather than quietly dropped.

## Sharing a board with someone — the actual sequence

The API has two ways to add a member and they are not interchangeable in practice, so this names the
one that works when you are sitting in front of a deployment.

**By invite (recommended).** Nobody has to look up or exchange an account id.

```bash
# 1. The owner mints an invite for one person, at one tier, valid for one use.
curl -sX POST https://wheel.example/v1/projects/$PROJECT/invites \
     -H "x-auth-token: $OWNER_TOKEN" -H 'content-type: application/json' \
     -d '{"role":"guest","email":"souren@example.com","expires_in_days":7}'
# → {"id":"…","role":"guest","expires_at":"…","token":"wi_…"}   the token is shown ONCE

# 2. Send them the `wi_…` token. They sign in (or sign up) on the same deployment, then:
curl -sX POST https://wheel.example/v1/invites/accept \
     -H "x-auth-token: $THEIR_TOKEN" -H 'content-type: application/json' \
     -d '{"token":"wi_…"}'
# → {"project_id":"…","role":"guest"}

# 3. It is now in their own project list, with the tier they were given.
curl -s https://wheel.example/v1/projects -H "x-auth-token: $THEIR_TOKEN"
```

Setting `email` locks the invite to that address, checked against the account's verified address
rather than anything in the request — so a leaked link is useless to anyone else.

**By account id.** Direct, and the right call when the owner already knows the id — for example
because they created the account themselves with `POST /v1/auth/users`:

```bash
curl -sX POST https://wheel.example/v1/projects/$PROJECT/members \
     -H "x-auth-token: $OWNER_TOKEN" -H 'content-type: application/json' \
     -d '{"user_id":"<their users.id>","role":"prompter"}'
```

There is deliberately **no lookup of an account by email**: an endpoint that turns an address into a
user id is an account-enumeration oracle, and the invite flow removes the need for one. `GET
/v1/auth/me` is how somebody finds their *own* id.

**Taking it back** is `DELETE /v1/projects/{id}/members/{user_id}`. It takes effect on the next
request, and any live events WebSocket that member holds is closed rather than left running — see
[What bounds a live WebSocket](#what-bounds-a-live-websocket).

## Membership and invites

```
GET    /v1/projects/{id}/members                    → { creator, creator_email?, members: [Member] }  (guest)
POST   /v1/projects/{id}/members  {user_id, role}   → Member                           (admin)
DELETE /v1/projects/{id}/members/{user_id}          → 204                              (admin)

GET    /v1/projects/{id}/invites                    → [InviteInfo]                     (admin)
POST   /v1/projects/{id}/invites                    → InviteInfo + token               (admin)
       {role, email?, expires_in_days?, max_uses?}
DELETE /v1/projects/{id}/invites/{invite_id}        → 204                              (admin)

POST   /v1/invites/accept  {token}                  → { project_id, role }             (any account)
```

An invite token is `wi_` plus 32 random bytes; only its SHA-256 is stored, so a copy of the database
is not a copy of anyone's invitations. It expires (7 days by default), has a use count (1 by
default), and may be locked to an email — checked against the account's *verified* address, never
against a claim in the request.

**Accepting an unusable invite is `404 invite_unusable`, never `401`.** Unknown, expired, revoked,
fully used, locked to another address, and "your membership of this project was revoked" all answer
the same status and body, so a link cannot be probed to learn which links exist. It is deliberately
not a 401: the caller's session is fine, and clients treat a 401 as "your login is dead" (the web app
clears the cookie), which would sign out a visitor for opening a stale link.

### Member and creator email — resolved for display, masked for a guest

`Member.email` and `MemberList.creator_email` are `Option<String>`, omitted from the response
entirely when there is nothing to show (`skip_serializing_if`, the same convention `Project.tier`
uses) — clients must handle absence, not assume the field is always there.

**Resolution.** `user_id`/`creator` are opaque principals — a `users.id` UUID under `local` auth, an
external provider's `sub` under `jwks` — and are never masked at any tier, since they identify
nothing about the person beyond "a member of this project," the same as a database row id would.
`email` is a best-effort DISPLAY value resolved separately:
- Under `local` auth, every member's principal IS a `users.id`, so the API looks up any row's email
  directly — not just the caller's own.
- Under `jwks`, there is deliberately no local account row for another provider's principal at all
  (members can be `jwks` accounts with nothing in this API's own `users` table). The only email this
  API can ever know for a `jwks` member is the CALLER'S OWN, read from the `email` claim on the JWT
  that authenticated the current request, when the provider includes one. Every other `jwks`
  member's `email` is always absent — not a gap to close, the honest limit of what a stateless
  verifier can know about someone else's account.

**Masking, guest tier only.** A guest sees every OTHER member's `email` (and `creator_email`, unless
they are the creator) masked: first two characters, a fixed three-character mask (`•••`, not
sized to the hidden length — a size-matched mask leaks the length), last two characters, domain
shown in full (`so•••ne@example.com`). A local part of four characters or fewer shows only its
first character plus the mask (`ab@x.com` and `abcd@x.com` are indistinguishable once masked, which
is the point — showing both characters of a two-character local part is not meaningfully masked at
all). Counted in Unicode code points, not bytes, so a multi-byte character is never split. A
non-email-shaped identifier, if one is ever surfaced through this same helper elsewhere, gets the
identical rule applied to the whole string.

**The guest's own row is never masked** — they already know their own email, and masking it would
cost them a lookup without hiding anything. Every other tier (`prompter`, `admin`) sees every email
unmasked; masking is guest-specific, not a general privacy filter.

**Enforced server-side**, in the handler, before the response is built — the same reasoning
`GET .../board`'s `redact_credentials` already uses for vault key names: a client that masked the
value itself would not protect a caller hitting this route directly (`curl`, a script, anything
that is not the reference UI).

Accepting is idempotent and **never lowers an existing tier**, so a stale guest link cannot be used
to demote a prompter. Unknown, expired, revoked and exhausted invites are one indistinguishable
answer: the link is a credential, so the response must not say which links exist.

Listing invites is admin, not guest: an invite's existence and tier are facts about who is about to
gain access.

### Redaction contract for membership data (decision, 2026-09-13)

**When a field in `Member` or `InviteInfo` is hidden from a caller's tier, it is PRESENT and emptied
(`null` / `""` / `[]` as the field's type requires), never absent, and the containing object carries a
sibling `"redacted": true`.** This is the same shape `RedactCredentials` already uses for a board
node's config (`wheel-core`'s `NodeConfig::redact_credentials`) — one redaction convention across the
whole API, not two, so a client needs exactly one parsing strategy wherever it sees `"redacted"`.

Reasons, independent of the precedent alone:

- **A required field cannot become absent without breaking strict deserialization.** `RedactCredentials`
  empties `VaultConfig.keys` rather than removing it for exactly this reason ("a board entry that fails
  to deserialize is worse than one that says nothing") — the same is true for a generated client's
  non-optional field.
- **Some of these fields are already legitimately `null` for a reason that has nothing to do with
  redaction** — `Member.invited_by` is `null` for a member with no recorded inviter, and always
  serializes as `"invited_by": null` today (it is `Option<String>` with no
  `skip_serializing_if`, so absence was never how "no value" was expressed here). If redaction
  ALSO used `null`, or used absence, a client could not tell "nothing to show" apart from "hidden from
  you" — an explicit signal is required either way, which is the actual argument for matching the
  existing shape rather than inventing a second one.
- **Disclosure, checked per field rather than assumed:** presence-with-`redacted:true` for
  `invited_by` tells a guest nothing they could not already infer (every member but the creator was
  invited by *someone*); the same holds for an invite's `expires_at` (every invite has one). Where it
  would not be a wash — e.g. whether an invite is email-locked, if that field is ever surfaced below
  admin — omission is actually the *worse* choice: an invite either has a locked `email` or does not,
  so omitting the field only on locked rows (to avoid saying "there's an email here, hidden") makes the
  row shape itself the leak, distinguishing locked from unlocked invites by structure instead of by the
  value the redaction was supposed to hide. Present-and-emptied has no such tell: every row keeps the
  same keys regardless of what is inside them.

Not a live gap today: `GET /v1/projects/{id}/members` is guest-visible and currently returns every
`Member` field unredacted by design (`routes/members.rs`: "a guest may see who else is here"), and
`GET .../invites` is admin-only at the *route* level, so there is no partial view of `InviteInfo` to
redact yet. This is the contract for whichever of the two changes first — a new field on `Member` that
should not be guest-visible, or a lower-tier view of invites — so it is decided once, in the open,
rather than by whichever PR happens to touch it first.

## Routes

### `GET /healthz`
Unauthenticated. `200 {"status":"ok","auth_mode":"local"|"jwks"|"external"}`.

`auth_mode` is the mode this API is actually running, and it is published so a client can assert it
agrees. If the web build ships `clerk` while the API runs `local`, the user gets a login widget whose
token we reject, or a form talking to a verifier that is not running — two correct halves, a
deploy-time disagreement, and nothing either side tests alone can see. The web smoke path compares
this against `NEXT_PUBLIC_AUTH_MODE` and goes red on a mismatch.

**The mode and nothing further.** Not the issuer, not the JWKS URL, not key material. Publishing the
mode reveals nothing that `POST /v1/auth/login` answering `401` rather than `404` does not already
reveal, and that argument covers the mode exactly. `tests/healthz_auth_mode.rs` holds the response to
those two keys, so a future field cannot be added to an unauthenticated probe by accident.

### `GET /v1/host/healthz`

Liveness of the sandbox host, as seen from the API. Unauthenticated, like `/healthz`.

```
200 {"ok": true}      the host is serving
503 {"ok": false}     it is not
```

It exists because `GET /healthz` answering 200 does not mean the product works: during one outage
the API stayed perfectly healthy while the host was stopped, and every project create hung until
the platform edge gave up. The host has no public domain, so nothing outside the API can ask it
directly.

Liveness only — no backend name, no project counts, no upstream error text. The answer is cached for
one second: the route is unauthenticated, and without that a flood here becomes a flood against the
one machine every tenant's sandbox runs on.

### `GET /v1/info`

Unauthenticated capability discovery for this API-layer build, mirroring `GET /v1/engine` one layer
down (`PROTOCOL.md`'s "Engine discovery"): what THIS layer can do, checked before a client depends on
it, rather than inferred from an incidental response field.

```jsonc
{
  "version": "0.1.0",     // CARGO_PKG_VERSION, compile-time
  "api_version": "v1",
  "features": ["membership"]
}
```

**Why this exists rather than an engine `FEATURES` id:** membership (`POST /v1/projects/{id}/members`
and friends, below) is enforced entirely in this crate's own auth/policy layer before a request ever
reaches a project's engine — `wheel-engine` has no route, config field or behaviour for it at all, so
there is nothing there a test could hold a `"membership"` id to. It would be a permanently-true string
with no engine-side truth behind it. This route is the discovery surface for capabilities that belong
to the API layer instead — `tests/info.rs` proves `membership` by calling a real membership route
against a real project, not by asserting the string is present in a const.

- **Additive only**, same rule as `GET /v1/engine`: a client ignores fields and feature ids it does
  not know. An absent id means the capability is not there.

| Feature id | What it guarantees |
|---|---|
| `membership` | `/v1/projects/{id}/members`, `/v1/projects/{id}/invites` and `/v1/invites/accept` exist and behave as documented under "Membership and invites" above. |

### `POST /v1/projects`
```bash
curl -X POST https://api.wheel.dev/v1/projects \
  -H "x-auth-token: $JWT" -H 'content-type: application/json' \
  -d '{"name":"my board"}'
```
`201` → `Project`. Name is 1–64 characters, no control characters. Generates the project's engine
secret and vault key, stores them encrypted (AES-256-GCM under `API_MASTER_KEY`), registers the
sandbox with the host, **and starts it** — a new project comes back `running` and its engine answers
immediately (`ARCHITECTURE.md` §6 M1: "create project → sandbox starts").

The create still succeeds if the sandbox does not come up: the row exists, so `201` is the honest
answer, and the returned `status` is `error` rather than `stopped`. Retry with `POST
/v1/projects/{id}/start`. A `status` of `starting` means the host accepted the start but the engine
had not reported healthy when we looked — poll `GET`.

### `GET /v1/projects`
`200` → `[Project]`, newest first. Only the caller's own projects. `x-project-id` not required.

### `GET /v1/projects/{id}`
`200` → `Project`, with `status` reconciled against what the host actually reports.

### `PATCH /v1/projects/{id}`
```json
{ "name": "renamed", "capabilities": { "http": true } }
```
Both fields optional. Setting `capabilities.http = true` is what opens the public ingress route.

### `DELETE /v1/projects/{id}`
`204`. Stops and destroys the sandbox and its data, then deletes the row. The sandbox is torn down
first: if that fails the row is kept, because a sandbox we no longer have a record of is one nobody
will ever clean up.

### `POST /v1/projects/{id}/start` · `/stop` · `/restart`
`200` → `Project`. `start` blocks until the engine reports healthy (up to ~30 s) or the host
returns a timeout.

Two failures are worth knowing by name, because both were once reported as something else:

- **The host is still restoring.** For a short window after the host restarts, project routes answer
  `503` and the body says how far through it is (`restored` of `to_restore`). A host working through
  its list and a host wedged on the first project used to be the same response; retry rather than
  treat it as an error.
- **The volume is full.** The host refuses a start below its free-space floor rather than launching
  a sandbox that will corrupt its own database on the first write. `POST /start` returns `507
  insufficient_storage`; a `POST /v1/projects` that hits it returns `201` with `status: "error"`,
  because the project was created either way. This is the failure that took production down once
  already, wearing a sqlite error about shared memory as a disguise, and it is reported by name so
  the next one is not a 500 with no cause.

### `ANY /v1/projects/{id}/engine/{*rest}`
Authenticated proxy to the project's engine control plane (`ARCHITECTURE.md` §4). Ownership is
proven before any byte is forwarded.

```bash
curl https://api.wheel.dev/v1/projects/$PID/engine/v1/board -H "x-auth-token: $JWT"
```

WebSocket upgrade is supported for `/engine/v1/events` and bridged in both directions. Frames are
relayed verbatim, without inspection or re-encoding — the `message` event in particular must reach
the UI unmodified so a row can be correlated by its id.

Header hygiene, both directions:
- Hop-by-hop headers are dropped, **including any header the client names in `Connection`**.
- The client's `x-auth-token` / `Authorization` never reach the host. The host authenticates the
  API, not the end user; relaying a user credential downstream is how replay bugs begin.
- `WHEEL_HOST_SECRET` is attached upstream and never appears in a response.

Two engine routes this proxy carries transparently, with nothing project-specific to add here — the
generic path above already covers them — but worth naming because Web's board shipped disabled
buttons pointing at exactly these gaps (PROTOCOL.md §"Agents", § "Script nodes"):

- `POST /v1/projects/{id}/engine/v1/agents/{agent_id}/interrupt` → `{status, session_id?}`. Cancels
  the turn an agent is in the middle of without losing its session — the smaller sibling of `stop`.
- `POST /v1/projects/{id}/engine/v1/scripts/{script_id}/run` → `{stdout, stderr, exit_code,
  timed_out, stdout_truncated, stderr_truncated}`. **Answers `503 config` on a deployment that has
  not set `WHEEL_SCRIPT_EXEC`** — script execution is gated off by default pending the per-node
  isolation work in `docs/proposals/script-execution-scope.md`; see PROTOCOL.md for why.

### Confirming which code a project's engine is running

`GET /v1/projects/{id}/engine/v1/healthz` (through the proxy above) returns the engine's own health,
including a `build` field:

```json
{"build":"4af95c8f0ba604c72481e98e7abc0d7725a2e0d4","ok":true,"stalled":[],"version":"0.1.0"}
```

**It is an owner-readable fact, not a public one.** The API's own `/healthz` and `/v1/host/healthz`
do not carry it — the latter deliberately, since it is liveness-only for an unauthenticated caller.
Reading `build` requires the project owner's token.

**What the number means depends on how the image was built**, and the difference matters at exactly
the moment you are relying on it:

| image built by | `build` reports |
|---|---|
| `make engine-image`, or any build passing `--build-arg GIT_SHA` (this is what CI does) | the exact commit the binaries were **compiled from** |
| Railway | `"unknown"` — it builds `docker/Dockerfile.host` directly and passes no build args, so `ARG GIT_SHA` keeps its default |

The value is baked at **compile** time (`option_env!` reading the build stage's `ENV`), so nothing at
runtime can change it. Verified by running the real image: with `--build-arg` the field matched
`git rev-parse HEAD` exactly, while the container's own `WHEEL_BUILD_SHA` was unset — the field is
right *because* it was compiled in, not because the environment supplied it.

So on today's production deploys `build` reads `"unknown"`. That is honest rather than misleading —
it declines to name a commit rather than naming the wrong one — but it means **`build` cannot yet
confirm a Railway deploy**. Confirm those by checking that the deploy actually rebuilt.

Making it exact in production requires passing `GIT_SHA` as a Railway build arg. Whether Railway
supports that has now been **verified: it does not**, by two mechanisms — see
`infra/railway/README.md`. So `build` reading `"unknown"` in production is the honest end state
rather than a gap awaiting a fix.

### `POST /v1/projects/{id}/builder/turns` — the Workflow Builder, streamed

**Admin tier.** A turn can name an agent's or a vault's credential as its source and its output is
only ever applied by an admin, so a guest or prompter gets `403`. Forwards the conversation to the
project's engine (`docs/PROTOCOL.md` §"Workflow Builder") and streams its Server-Sent Events back
verbatim. The builder's own LLM account, `GET|PUT|DELETE .../engine/v1/builder/credential`, is admin
for every method, through the proxy's tier table (`auth/policy.rs`).

```jsonc
POST /v1/projects/{id}/builder/turns
{ "mode": "new" | "improve",
  "turns": [ {"role": "user"|"builder", "text": "…"} ],
  "credential": {"source": "builder"} }        // or agent/vault, see PROTOCOL
```

A route of its own rather than the `/engine/{*rest}` wildcard for a measured reason: the shared
upstream client carries a 30s whole-request timeout (`proxy_timeout_secs`), which would cut a turn
off mid-answer. This route allows the turn the engine's own 240s ceiling plus the hops, so what a
caller sees is the engine's `timeout` frame rather than a severed connection. `cache-control:
no-transform` and `x-accel-buffering: no` travel with it so no hop buffers the stream into one late
response.

Frames are `delta`, then `done` or `error` — the engine's contract, passed through unchanged. A
refusal that happens **before** the stream opens is ordinary JSON with its status (`409 needs_auth`
carrying the credential sources, `403 policy`, `429 builder_busy`, `400`, `413`), because there is
nothing to stream yet and the caller has to answer it.

### `POST /v1/mcp` — the operator MCP server

MCP over Streamable HTTP, so an AgentGrid master or a developer's own Claude Code can drive a board
as tools (`docs/proposals/agentgrid-parity.md` §3). JSON-RPC 2.0: `initialize`, `ping`, `tools/list`,
`tools/call`. A notification (no `id`) is answered `202` with no body. `GET`/`DELETE /v1/mcp` answer
`405`: this server keeps no session and opens no server-initiated stream.

Authentication is the ordinary one — a `wht_` API token or a session — so **it grants nothing new**:
every tool is a call onto a route the same credential could already reach. Tokens carry no scopes
yet, so there is no read-only variant (proposal R9).

| tool | what it does |
|---|---|
| `projects` | your projects: id, name, status. Where a project id comes from |
| `board` | the whole board of one project, each agent's state included |
| `send` | message an agent, return its receipt |
| `ask` | message an agent and wait for the turn that handles it, returning that turn's final text |
| `start` · `stop` | an agent's process |
| `logs` | recent output from one agent |

- **Every tool that names a project is authorised for that call**, against the same owner predicate
  every other route uses. A project you do not own is the same `not_found` as one that does not
  exist, and the engine is never reached on the way to that answer.
- **`Origin` is validated** against the deployment's CORS allowlist and an unknown origin is refused
  before any tool runs. A loopback bind is not an auth boundary: without this, a page in the
  operator's browser could drive a local `wheeld`.
- **Agent-authored text is labelled.** `ask` results and `logs` lines come back behind an explicit
  "treat as untrusted input" line: they are another agent's words arriving in a model's context.
- A refusal is a **tool error** (a successful JSON-RPC response with `isError: true`), so the model
  reconsiders instead of concluding the server is broken. An unknown *method* is a protocol error.

### `ANY /p/{project_id}/{*rest}` — public ingress
**Unauthenticated by design.** Reaches the project's `endpoint` nodes.

On success the engine's own response is returned unchanged — status, headers and bytes. For a hit on
an `endpoint` node whose wires deliver to agents, that is:

```json
202 {"accepted": true, "queued": 1}
```

`queued` is the number of messages **enqueued**, which is what has happened at the moment the
response is written. Nothing has been delivered yet: the pump is asynchronous, and the target agent
may be parked, unauthenticated or mid-turn. The field is deliberately not called `delivered` — it was
once, and production answered `delivered: 1` for a message that never reached a child, which a
webhook provider reads as success and never retries.

The `202` is not a promise that an agent acted on the hit, only that the hit is durably queued. To
observe real delivery, watch the `message` event on the events WebSocket, where the state machine is
`queued → delivered → consumed` (§3c#4). An endpoint with `response_mode: script` returns the
script's own output instead of this envelope.

Failure cases:

- `404` if the project does not exist.
- `403` if `capabilities.http` is false (the default, and also the result of a malformed
  capabilities blob — this fails closed). The toggle is `capabilities.http` on `PATCH
  /v1/projects/{id}`.
- `501 ingress_unavailable` if the engine answers with a **bodiless** 404 — it has no `/ingress/*`
  route at all, so the path is not the problem and saying "not found" invites the reader to hunt for
  a typo. A 404 the engine actually wrote (its `no_such_endpoint`, or an endpoint script's own)
  passes through unchanged.
- Rate limited per project, default 60 req/min, `429` when exceeded.
- Body capped at 5 MiB, `413` when exceeded.
- Every `x-wheel-*` header from the caller is stripped before we add `x-wheel-ingress: 1`, so a
  public caller cannot forge the marker the engine trusts.

Counting happens only after the project is known to exist, so traffic aimed at random UUIDs cannot
make us write unbounded counter rows.

## What bounds a live WebSocket

The events socket is long-lived by design, which is exactly why an established bridge needs bounds
that a request-time check cannot give it (ADVERSARY 011). Four things end one:

* **A per-project cap** (`WS_MAX_BRIDGES_PER_PROJECT`) refuses a new bridge before it is opened, so a
  project at its ceiling cannot make the API open sockets to the host to find that out. On the
  production `process` backend every tenant shares one machine and one file-descriptor budget, so
  this bounds the blast radius regardless of the idle story. **Per replica** — see the table above.
* **A keepalive with a pong deadline.** The server pings every 30 s; a peer that does not answer
  within the interval is closed. A plain idle-read timeout cannot tell a dead peer from a
  legitimately idle one on a channel that is silent whenever nothing is happening.
* **A membership re-check**, every 30 s and on notification. A revoked *or downgraded* member's
  socket closes rather than surviving until it happens to end. On Postgres a `NOTIFY` makes that
  near-immediate across replicas; the periodic check is what makes it certain when the notification
  is missed, and it works on both backends.
* **An absolute lifetime cap** (`WS_MAX_LIFETIME_SECS`). Defence in depth: it bounds how long a
  missed revocation can persist even if the notification and the re-check both fail.

Not closed here, and worth knowing: **the authenticated HTTP proxy still has no rate limit** — only
public ingress does. One authenticated tenant can flood proxy → host → engine. That is the second
half of ADVERSARY 011 and needs a shared per-project counter like the ingress limiter.

## Rate limiting across replicas

The limiter is a fixed-window counter in Postgres, not an in-process bucket. With N replicas behind
a load balancer an in-memory limit silently becomes N × the configured value — the control weakens
exactly as you scale, which is backwards. The window boundary is computed with the *database's*
clock so replicas agree.

Known tradeoff: a fixed window admits the classic boundary burst, up to 2× the limit across two
adjacent windows. Accepted for v1 — this exists to stop sustained abuse of an unauthenticated
route, not to smooth traffic. A sliding window in Redis is the upgrade path.

## Configuration

| Variable | Required | Default | Notes |
|---|---|---|---|
| `WHEEL_ENV` | no | `prod` | `dev` or `prod`. Anything else refuses to boot. Unset means prod. |
| `STORE` | yes* | — | `postgres://…` or `sqlite://path/to/wheel.db`. The scheme picks the backend, so there is no mode flag that can disagree with the connection string. |
| `DATABASE_URL` | yes* | — | Accepted as an alias for `STORE`; production, the compose stack and every deploy already set it. (*one of the two is required.) |
| `WHEEL_JWKS_URL`, `WHEEL_JWKS_ISSUER` | under `jwks` | — | Where `AUTH_MODE=jwks` fetches the provider's signing keys, and the `iss` it pins. |
| `WHEEL_JWKS_AZP` | no | — | Comma-separated `azp` allowlist. |
| `CLERK_JWKS_URL`, `CLERK_ISSUER`, `CLERK_AZP` | no | — | **Deprecated aliases** for the three above. Still read, with a one-line boot warning naming both spellings. Setting a name and its alias to *different* values refuses to boot. See [The `CLERK_*` aliases](#the-clerk_-aliases). |
| `API_MASTER_KEY` | yes | — | 32 bytes, base64. `openssl rand -base64 32`. |
| `WHEEL_HOST_URL` | yes | — | e.g. `http://wheel-host.railway.internal:7100`. |
| `WHEEL_HOST_SECRET` | yes | — | Bearer for the host. Must never appear in a sandbox's environment. |
| `AUTH_DEV_SECRET` | no | — | HS256 test tokens. **Only honoured when `WHEEL_ENV=dev`.** |
| `CORS_ALLOWED_ORIGINS` | no | empty | Comma-separated exact origins. Empty means no browser may call the API directly: the web UI calls it from its own server. |
| `MAX_PROJECTS_PER_USER` | no | `20` | |
| `INGRESS_RATE_PER_MIN` | no | `60` | `0` disables. |
| `INGRESS_BODY_LIMIT_BYTES` | no | `5242880` | |
| `PROXY_TIMEOUT_SECS` | no | `30` | Not applied to WebSockets or log streams. |
| `PUBLIC_BASE_URL` | no | `http://localhost:8080` | The public base of every `ingress_base_url` (`<PUBLIC_BASE_URL>/p/<id>`, returned by every project route, including create), and the issuer of local sessions. Clients display it as-is: the web app no longer builds it. Behind TLS, `https://<domain>`. Changing it ends every session: the issuer moved. `wheeld` defaults it to `http://localhost:<port>` of its bind. |
| `WHEEL_SIGNUP` | no | `closed` | `closed` or `open`, local auth only. Unset or empty is closed. See [Signup policy](#signup-policy-wheel_signup). |
| `WHEEL_TRUSTED_PROXIES` | no | none | Comma-separated addresses or CIDRs of reverse proxies whose `X-Forwarded-For` is believed. See [Behind a reverse proxy](#behind-a-reverse-proxy). A malformed entry refuses to boot. |
| `HOST_CONNECT_TIMEOUT_SECS` | no | `3` | How long to wait for a TCP connection to the host before calling it unreachable. Separate from `PROXY_TIMEOUT_SECS` on purpose — see below. |
| `WS_MAX_BRIDGES_PER_PROJECT` | no | `16` | Live WebSocket bridges one project may hold **on this replica**. Per replica, not global: with N replicas the effective ceiling is N times this. It is a blast-radius bound, not a quota (ADVERSARY 011). |
| `WS_MAX_LIFETIME_SECS` | no | `3600` | Absolute lifetime of a bridge; the client then takes a new ws-ticket. |

### `AUTH_MODE=external` — the external verifier's variables

Everything the verifier does is named by a variable. Nothing is inferred and nothing has a
permissive default. **Setting any `WHEEL_EXTERNAL_*` variable while `AUTH_MODE` is not `external`
refuses to boot, naming the variable** — a knob that looks configured and is never read is how a
deployer comes to believe they have pinned an audience.

| Variable | Required | Default | Notes |
|---|---|---|---|
| `WHEEL_EXTERNAL_VERIFIER` | yes | — | `jwks` or `proxy_header`. Any other value refuses to boot rather than falling back to either real one. |
| `WHEEL_EXTERNAL_ISSUER` | yes | — | Exact `iss` to pin. Under `proxy_header` it is a synthetic stable label (e.g. `proxy:oauth2-proxy`), because the principal key needs an issuer even when no token exists. |
| `WHEEL_EXTERNAL_AUDIENCE` | yes | — | Comma-separated. **Exact string equality**, never a prefix. Empty or blank refuses to boot. Must name *this Wheel deployment*, never the issuer origin. |
| `WHEEL_EXTERNAL_ALGS` | yes (`jwks`) | — | Comma-separated allowlist, e.g. `RS256,EdDSA`. Case-insensitive. A symmetric algorithm (`HS*`) is refused **by name** at boot. An algorithm no key can ever carry (`ES256`) is refused too, rather than booting and then rejecting every token for a reason nobody can see. |
| `WHEEL_EXTERNAL_JWKS_URL` | yes (`jwks`) | — | Subject to the production identity-provider interlock: `https://` and non-local in prod. |
| `WHEEL_EXTERNAL_PROVISION` | yes | — | `auto` or `linked`. No default: if your issuer lets anyone sign up, `auto` lets anyone into Wheel, so this is a decision to make out loud. |
| `WHEEL_EXTERNAL_PROVIDER` | no | `external` | The operator's display label on `external_identities` rows. Never a link key. |
| `WHEEL_EXTERNAL_TOKEN_HEADER` | no | `x-auth-token` / `Authorization: Bearer` | Read the credential from a different header, e.g. `cf-access-jwt-assertion` for Cloudflare Access. Stored case-folded. |
| `WHEEL_EXTERNAL_SUBJECT_CLAIM` | no | `sub` | Point at a better immutable identifier where the IdP has one (`oid` on Entra). Empty refuses to boot. |
| `WHEEL_EXTERNAL_AZP` | no | — | `azp` allowlist. When set, a token with **no** `azp` cannot satisfy it. |
| `WHEEL_EXTERNAL_MAX_TTL_SECS` | no | none | Requires `iat` and refuses `exp - iat` above it. Must be a positive whole number. Unset means no cap — and no cap means revocation latency equals token lifetime. |
| `WHEEL_EXTERNAL_SOLE_AUDIENCE` | no | off | `1`/`true`/`yes` requires our audience be the **only** one the token names. |
| `WHEEL_EXTERNAL_ALLOW_ISSUER_AUDIENCE` | no | off | `1`/`true`/`yes` permits an audience equal to the issuer's own origin, which otherwise **refuses to boot**. Only for an issuer that mints tokens for Wheel and nothing else. |
| `WHEEL_EXTERNAL_PROXY_SUBJECT_HEADER` | yes (`proxy_header`) | — | e.g. `x-forwarded-user`. Stored case-folded. Stripped at the proxy hop on every outbound path. |
| `WHEEL_EXTERNAL_PROXY_EMAIL_HEADER` | no (`proxy_header`) | — | Display only, never a link key. Stripped at the hop alongside the subject header. |

Under `proxy_header`, `WHEEL_TRUSTED_PROXIES` becomes **required**: an empty list refuses to boot.

### Cloudflare Access is a `jwks` deployment, not a `proxy_header` one

It passes a *signed* JWT in `Cf-Access-Jwt-Assertion` and publishes a JWKS. "Where the credential is
read from" and "how it is verified" are two axes, not one, and they are configured independently:

```
WHEEL_EXTERNAL_VERIFIER=jwks
WHEEL_EXTERNAL_TOKEN_HEADER=cf-access-jwt-assertion
```

Treating Access as a trusted-header provider would throw away a signature we could check.

### Running `jwks` or `external` without a provider account — the stub issuer

```
cargo run -p wheel-api --example stub-issuer          # port 9911
PORT=9000 SUB=user_abc cargo run -p wheel-api --example stub-issuer
```

It serves **two** JWKS documents, because there are two modes and they are deliberately not the same
issuer — `Config::cross_check` refuses to boot with two verifiers pinned to one issuer.

| Path | Keys | For |
|---|---|---|
| `/jwks` | RSA only | `AUTH_MODE=jwks` |
| `/external/jwks` | RSA **and** Ed25519 | `AUTH_MODE=external` |

Both algorithms on the external plane, because the whole point of the external verifier is that the
algorithm comes from the key rather than from the token, and a key set with one algorithm in it
cannot demonstrate that.

```
AUTH_MODE=jwks
WHEEL_JWKS_URL=http://127.0.0.1:9911/jwks
WHEEL_JWKS_ISSUER=https://clerk.example.test
```

```
AUTH_MODE=external
WHEEL_EXTERNAL_VERIFIER=jwks
WHEEL_EXTERNAL_JWKS_URL=http://127.0.0.1:9911/external/jwks
WHEEL_EXTERNAL_ISSUER=https://idp.example.test
WHEEL_EXTERNAL_AUDIENCE=wheel-stub
WHEEL_EXTERNAL_ALGS=RS256,EdDSA
WHEEL_EXTERNAL_PROVISION=auto
```

`WHEEL_ENV=dev` is required for either of those URLs: the production identity-provider interlock
refuses a loopback issuer, which is exactly what this is.

`GET /token?sub=<id>&alg=<RS256|EdDSA>&aud=<audience>` mints more. Passing `aud` mints on the
external plane (external issuer, audience set); omitting it mints on the `jwks` plane. `PORT` and
`SUB` override the defaults, and the process prints a ready token for each plane at startup.

**It verifies itself before it serves.** On startup it mints tokens on both planes and both
algorithms and puts them through `auth::claims::verify` *and* `auth::external::verify_token` — the
real production verifiers, not copies — and **exits non-zero** if any of them fails. So a drift
between a JWKS shape, a claim set and what the API accepts is caught here rather than somewhere less
obvious. That is also what keeps the test fixture and the runnable stub from diverging: both sign
with the same two committed keys, in `crates/wheel-api/tests/fixtures/`.

**Never in production.** It is an `examples/` target, so it is in no shipped binary and unreachable
from the library, and its signing keys are committed to this repository in plain text — anyone can
mint any `sub`, on either plane.

### The dev-bypass interlock

`AUTH_DEV_SECRET` accepts HS256 tokens, which anyone holding the secret can mint for any `sub`. It
is a complete authentication bypass, deliberately, for local testing.

**If it is set while `WHEEL_ENV` is not `dev`, the process refuses to start.** An unset `WHEEL_ENV`
counts as prod, because in practice nobody sets `WHEEL_ENV=prod` by hand — they just don't set it,
and that must not be the permissive case. Covered by `tests/config_interlock.rs`.

### Why connecting and responding have different timeouts

A slow *response* is normal: a project start legitimately blocks while an engine boots. A slow
*connect* means the host is not there, and waiting does not help. They were once the same 30s, and
when the host went down, `POST /v1/projects` sat for the full request timeout until the platform
edge returned its own 502 — so the browser got an edge error page instead of our error envelope and
the UI simply hung. Connect now gives up after `HOST_CONNECT_TIMEOUT_SECS`, and the outage is
reported as a project in `error` state with a body the client can read.

### Two backends, one API

Postgres is production. SQLite exists so a local or open-source install has no dependency to stand
up first — it is what `wheeld` uses by default, and it is a real backend rather than a stub: the
same routes and the same schema, translated in `migrations_sqlite/`.

How much parity is actually proven, stated precisely because "both backends are tested" is the kind
of claim that gets believed:

* `sqlite_parity.rs` drives the **real router** against SQLite — signup, login, `/auth/me`, logout,
  password change, project create/list/rename, the per-user cap, the cross-tenant 404, the shared
  login limiter and the maintenance sweeps. It needs no `TEST_DATABASE_URL`, so it runs everywhere.
* `ratelimit_db.rs` runs its assertions against both backends in one pass.
* `sqlite_store.rs` and `sqlite_dialect.rs` pin the schema and the dialect assumptions the shared
  SQL rests on, against a real database.
* The remaining `*_db.rs` suites are **Postgres-only** and skip without `TEST_DATABASE_URL`. Their
  SQLite coverage is whatever `sqlite_parity.rs` reaches, which is the routes above and not more.

Where the SQL differs it is written out per dialect, with `Db::pick` choosing between two named
statements rather than a string being assembled. The differences are deliberate rather than
incidental:

* **The rate limiters.** The window boundary comes from the *database* clock on both, because the
  API runs as N replicas whose own clocks may differ by seconds and a boundary they disagree about
  is a limit that admits more than it says. Postgres truncates with `date_trunc`; SQLite has no
  such function and uses `strftime` to the same effect.
* **Timestamps stay on the database side.** SQLite has no `now()`, and its `CURRENT_TIMESTAMP` is a
  different text format from the one the driver writes — comparing against that would be a
  lexicographic accident rather than a comparison. The SQLite statements use
  `strftime('%Y-%m-%dT%H:%M:%fZ', 'now')`, which produces exactly the format stored, so both
  backends keep reading the clock the rows were written against.

A duplicate-email signup is a 409 on both, which needs saying because the two report the constraint
differently (`23505` vs `2067`/`1555`); without that mapping a local install would answer 500 where
production answers 409, and "works locally" would stop meaning anything.

### The production identity-provider interlock

Under `WHEEL_ENV=prod`, the issuer and key-set URLs of **both** JWKS-backed modes must be `https://`
and must not point at loopback, RFC1918, link-local, unique-local IPv6, or a `.local` / `.internal`
name. Otherwise the process refuses to start. That is `WHEEL_JWKS_URL` and `WHEEL_JWKS_ISSUER` under
`AUTH_MODE=jwks`, and `WHEEL_EXTERNAL_JWKS_URL` and `WHEEL_EXTERNAL_ISSUER` under
`AUTH_MODE=external`.

This is not tidiness. A stub identity provider does not fail closed — it authenticates everyone, as
whoever the caller claims to be, and the ownership checks then work perfectly against an identity
the attacker chose (ADVERSARY 017, where a mock-auth build resolved every token to a single
`owner_id`). Boot is the only place to catch it. Dev is unaffected: pointing at a local issuer is
exactly what dev is for. The host is checked as a literal, without DNS — boot is not the place to
trust a resolver, and a name that resolves publicly today may not tomorrow.

## Configuration interlocks

Every one of these is a refusal at boot, not a warning, and every one is pinned by
`tests/config_interlock.rs` or `tests/external_config.rs`. The shared principle: a security control
that is half-configured must stop the process, because the alternative is a deployment that starts,
looks configured, and is not.

| Interlock | Why |
|---|---|
| `AUTH_MODE` unset in prod | Guessing wrong either rejects every real user or accepts tokens from the wrong issuer. |
| `AUTH_DEV_SECRET` set while `WHEEL_ENV != dev` | A total authentication bypass. See [the dev-bypass interlock](#the-dev-bypass-interlock). |
| `AUTH_MODE=jwks` with an empty `WHEEL_JWKS_URL`/`WHEEL_JWKS_ISSUER` | A placeholder that looks like configuration starts and then rejects every token for a reason nobody can see. |
| A `WHEEL_JWKS_*` name and its `CLERK_*` alias set to **different** values | There is no correct way to pick a winner between two issuers an operator has named, and picking one silently is how a deployment ends up pinned to a provider nobody meant. |
| Any `WHEEL_EXTERNAL_*` set while `AUTH_MODE != external` | A knob that looks configured and is never read is how a deployer comes to believe they have pinned an audience. The message names the variable. |
| `WHEEL_EXTERNAL_AUDIENCE` empty or blank | An unvalidated audience means a token minted for a different relying party is accepted here as its subject. |
| `WHEEL_EXTERNAL_ALGS` naming a symmetric algorithm | The key that verifies is then also a key that mints: anyone who can verify can forge. Refused **by name**, so an operator cannot believe they enabled something. |
| `WHEEL_EXTERNAL_ALGS` naming an algorithm no key can carry (e.g. `ES256`) | It would boot and then reject every token for a reason nobody can see. |
| `WHEEL_EXTERNAL_VERIFIER` unrecognised | It must not fall back to either real verifier. |
| `WHEEL_EXTERNAL_PROVISION` unset | `auto` on an open-signup IdP lets anyone into Wheel. Never a default. |
| `WHEEL_EXTERNAL_MAX_TTL_SECS` not a positive whole number | |
| `WHEEL_EXTERNAL_SUBJECT_CLAIM` empty | |
| `WHEEL_EXTERNAL_ISSUER == WHEEL_JWKS_ISSUER` | Two verifiers pinned to one issuer are two token populations that can stand in for each other. |
| `WHEEL_EXTERNAL_ISSUER == PUBLIC_BASE_URL` | That is the issuer of this API's own sessions. A local session JWT must never route to the external verifier, nor the reverse. |
| `WHEEL_EXTERNAL_VERIFIER=proxy_header` with an empty `WHEEL_TRUSTED_PROXIES` | Believing a header from everyone is not a configuration, it is an open door. |
| Prod, either JWKS mode, a loopback or plaintext issuer | [The production identity-provider interlock](#the-production-identity-provider-interlock). |

Two things **warn** rather than refuse, because Wheel cannot decide them for a deployer:

- `WHEEL_EXTERNAL_AUDIENCE` equal to the pinned issuer or its origin. Wheel does not know what else
  the deployer's issuer serves. The warning names both values and says what to use instead.
  (`ExternalAuth::audience_shadowing_the_issuer` is a pure function so a test can assert the rule —
  a rule that exists only inside a `tracing::warn!` cannot be asserted, and a control nobody can
  test is a control that silently stops firing.)
- `proxy_header` mode at all, naming the mode, because the reduced posture belongs in the first
  screen of logs.

### The `CLERK_*` aliases

`AUTH_MODE=jwks` predates this project having more than one identity provider, and its variables
were named after one vendor. Nothing in the mode is Clerk-specific — it is an OIDC issuer and a
JWKS. The documented names are now:

```
WHEEL_JWKS_URL     <- CLERK_JWKS_URL
WHEEL_JWKS_ISSUER  <- CLERK_ISSUER
WHEEL_JWKS_AZP     <- CLERK_AZP
```

**Aliased, not renamed.** `CLERK_JWKS_URL` and `CLERK_ISSUER` are *required* under `jwks` and an
empty value already refuses to boot, so a hard rename would turn a documentation fix into a failed
deploy at the next restart of every deployment configured the old way — including
`infra/docker-compose.yml`. The deprecated spelling keeps working and logs a one-line deprecation at
boot naming both variables.

The one hazard an alias introduces is ambiguity, and that is closed: **both names set to different
values refuses to boot**, naming both. Both set to the same value is a redundancy, not an error, and
an empty or whitespace value is an unset one. Dropping the alias is its own change, taken
deliberately once no deployment reads them.

## Behind a reverse proxy

Behind a proxy, the TCP peer is the proxy, and the caller's address is whatever `X-Forwarded-For`
says. A client can write that header itself. So:

- **`X-Forwarded-For` is believed only from a peer inside `WHEEL_TRUSTED_PROXIES`.** The default
  trusts no one, and then the peer is the client.
- **The client is the first address, counting from the right, that is not a trusted proxy.** A value
  a client prepended is never reached. A hop that is not an address ends the walk at the last one that
  could be vouched for.
- **A public ingress hit carries that address to the engine as `x-wheel-client-ip`.** The caller's
  own `x-wheel-*` headers are stripped first, so only the API can set it. The engine's per-caller
  ingress limit and an endpoint's `ip_allow` key on it.
- **`X-Forwarded-Proto` is never read.** The scheme the API advertises comes from `PUBLIC_BASE_URL`.

Both the `wheel-api` binary and `wheeld` apply this, and both are served with the peer address
available to it. For a proxy on the same machine, `WHEEL_TRUSTED_PROXIES=127.0.0.1/32,::1`.

**Trusting loopback trusts every process on the machine, agents included.** A native agent can connect
over loopback and claim any client address for a hit on another project's `/p/…`, which defeats that
endpoint's `ip_allow` and per-caller limit. A dedicated loopback alias does not change that, since any
local process may bind the alias as its source. Prefer the Docker layout, where the trusted address
is the proxy's own container, on a network the agents are not on.

Under `AUTH_MODE=external` with `WHEEL_EXTERNAL_VERIFIER=proxy_header`, `WHEEL_TRUSTED_PROXIES` stops
being an `X-Forwarded-For` policy and becomes **the authentication boundary**: the peer check is the
entire control, so the warning above is no longer about a rate-limit key, it is about who may be
anyone. An empty list refuses to boot in that mode, and trusting loopback there means every local
process — agents included — can authenticate as any subject. See
[`proxy_header`](#proxy_header--the-dangerous-mode-and-what-contains-it).

## Cookies are never credentials

The API reads a credential from `x-auth-token` or `Authorization: Bearer` and from nothing else.
There is no cookie path, now or ever. This is pinned by `tests/no_cookie_auth.rs`: a valid session
JWT or `wht_` token placed in a cookie gets `401` on every route, and a cookie-only logout revokes
nothing.

This matters behind the VPS proxy. `/v1` shares an origin with the web app there, so browsers attach
the web's `__Host-wheel_session` cookie to every `/v1` request, including ones a hostile page
causes. A cookie that authenticated would be ambient authority over the whole API. The VPS kit also
strips `Cookie` on `/v1` and `/p` at the proxy, as a second line; this rule is the first.

## CORS

The web UI calls the API from its own server, so its origin needs no CORS grant. The default
allow-list is empty, and the VPS deployment sets none. `CORS_ALLOWED_ORIGINS` exists for a browser
client someone is deliberately developing against the API directly.

Explicit origin allowlist from `CORS_ALLOWED_ORIGINS`. Never wildcard-with-credentials: the web app
authenticates with a header rather than cookies, so `allow_credentials` is never needed, and an
explicit list keeps a hostile page from scripting the API with a user's token.

**Methods and headers are mirrored from the preflight, not listed.** The origin allowlist is the
boundary; a hand-kept method list is a second copy of what the router serves, and it drifted — the
vault write is a `PUT`, `PUT` was missing, and the operator saw "Can't reach the API" rather than
anything naming a method or a route. A method the router does not serve now gets a `405` with a
body, which is readable; a preflight failure is not. `crates/wheel-api/tests/cors.rs`
(**API-cors-covers-every-served-method**) reads the routes out of the router's own source and holds
the preflight to every method each one accepts, so a return to a static list fails CI.

`ANY /p/{project_id}/{*rest}` carries its own permissive CORS (`Access-Control-Allow-Origin: *`, no
credentials): an ingress URL is public by definition, so restricting which page may read the reply
protects nothing and only stops the board's own "test this endpoint" button from showing it.

## Running the API natively (no Docker) — for the web team

The containerised stack rebuilds the Rust image on every change, which is a poor inner loop for
frontend work. This runs the same two binaries directly against the stub engine.

```bash
cargo build -p wheel-api -p wheel-host

# 1. Postgres (any local instance; create a database first)
#    docker run -d --name wheel-pg -p 55432:5432 \
#      -e POSTGRES_USER=wheel -e POSTGRES_PASSWORD=wheel -e POSTGRES_DB=wheel_dev postgres:17-alpine

# 2. Stub engine on :7000
python3 infra/dev/stub_engine.py &

# 3. Host on :7100
WHEEL_ENV=dev SANDBOX_BACKEND=external ENGINE_BASE_URL=http://127.0.0.1:7000 \
WHEEL_HOST_SECRET=dev-host-secret-at-least-16-chars BIND_ADDR=127.0.0.1:7100 \
WHEEL_DATA_DIR=/tmp/wheel-host-data ./target/debug/wheel-host &

# 4. API on :8080
WHEEL_ENV=dev BIND_ADDR=127.0.0.1:8080 \
DATABASE_URL=postgres://wheel:wheel@127.0.0.1:55432/wheel_dev \
WHEEL_JWKS_ISSUER=https://dev.wheel.local AUTH_DEV_SECRET=dev-only-hs256-secret \
API_MASTER_KEY=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA= \
WHEEL_HOST_URL=http://127.0.0.1:7100 WHEEL_HOST_SECRET=dev-host-secret-at-least-16-chars \
WHEEL_SIGNUP=open CORS_ALLOWED_ORIGINS=http://localhost:3000 ./target/debug/wheel-api
```

`SANDBOX_BACKEND=external` points the host at an engine someone else started, instead of creating
containers. It refuses to load unless `WHEEL_ENV=dev`, because it performs no isolation at all.

### Minting a token without an identity provider

With `AUTH_DEV_SECRET` set and `WHEEL_ENV=dev`, the API accepts HS256 tokens. `sub` is the user id,
so two different `sub` values are two different tenants — which is how to exercise the ownership
boundary locally. Claims: `{sub, iss, exp, nbf}`, where `iss` must equal `WHEEL_JWKS_ISSUER` (or the deprecated `CLERK_ISSUER`) exactly.
A ten-line reference implementation lives in `infra/dev/e2e.py` (`mint()`); copy it rather than
rewriting it.

Sanity check the whole chain, including the boundary cases, with:

```bash
python3 infra/dev/e2e.py
```

### Opening the events WebSocket

Browsers cannot set headers on a WebSocket handshake, and the session JWT must never appear in a
URL where it would be captured by proxy and server logs. So the socket is opened with a ticket:

```
POST /v1/projects/{id}/ws-ticket        -> { "ticket": "...", "expires_in": 30 }
ws://localhost:8080/v1/projects/{id}/engine/v1/events?ticket=<ticket>
```

The ticket is single-use, expires in 30 seconds, and is bound to the (user, project) pair it was
minted for.

### Reconnecting — there is no replay, on purpose

The events socket carries no history. `GET /v1/board` on connect is the snapshot; the socket only
ever adds to it from that point on (PROTOCOL.md § Events). Two situations both reduce to the same
rule, and a client that only handles one of them will silently drift on the other:

1. **The socket itself reconnects** (network blip, the API replica restarting, the ticket path's
   30 s window). Nothing to negotiate — resubscribing starts a fresh stream with nothing before it.
2. **A subscriber falls behind mid-connection.** The engine's fan-out never blocks the supervisor
   for a slow reader (PROTOCOL.md), so a reader that cannot keep up is dropped and told so with a
   `{"type":"lagged","hint":"events were dropped; refetch GET /v1/board"}` frame rather than being
   silently starved.

**In both cases: `GET /v1/board` is the only correct response.** Refetch it and reconcile every
node's `state` from the response — not just the ones a client happens to remember changing — because
whatever arrived during the gap is gone and unrecoverable from the socket itself.

**Per-agent logs are the one piece with a real resume cursor, and it is a *separate* mechanism from
the socket:** `GET /v1/agents/:id/log?since=<seq>` (PROTOCOL.md §"Agents") returns `{lines, next}`,
where `next` is the cursor to pass back in to continue exactly where a prior read left off — no gap,
no overlap. A client that seeds a log view once from `since=0` and then relies on the `log` WS event
for everything after has covered the *initial* load, but not a lagged/reconnected socket: nothing
re-seeds the gap the drop just created, so the visible log silently stops matching the engine's own
`logs` table until the tab is reloaded. The fix is mechanical — on `lagged`/resync, call `log` again
with `since` set to the last `seq` a client already has for that agent, for every agent whose log is
currently open, and append the result — but it touches the same reconnect path the board-refetch
lives on, so whether it lands as one PR with that path or two is a question for whoever owns it.

## Local development

```bash
export API_MASTER_KEY=$(openssl rand -base64 32)
export WHEEL_JWKS_URL=... WHEEL_JWKS_ISSUER=...
docker compose -f infra/docker-compose.yml up --build
```

The docker socket is mounted into the **host** service only. Anything that can reach the socket can
trivially escape to the machine, so the internet-facing API must never see it.

With `WHEEL_ENV=dev` and `AUTH_DEV_SECRET` set, HS256 tokens are accepted and the JWKS endpoint is
never contacted. `iss` is still validated, so a minted token must carry exactly `WHEEL_JWKS_ISSUER`.

### End-to-end check

`infra/dev/e2e.py` mints a dev token and walks the whole chain: create project → start sandbox →
read the board back through the authenticated proxy. It also asserts the boundary holds — no token
is `401`, another user's project is `404` (never `403`, which would confirm existence), and ingress
on a project that has not opted in is `403`.

```bash
python3 infra/dev/e2e.py
```

Until SDK's engine lands, `infra/dev/Dockerfile.engine.stub` provides a stub that implements just
enough to prove the chain: an unauthenticated `/healthz` for the host's readiness probe and a
bearer-gated `/v1/board`. `infra/dev/Dockerfile.host.dev` likewise builds only the supervisor and
is replaced by SDK's `docker/Dockerfile.host` when that exists.
