# External authentication for Wheel — proposal (multiplayer M0)

SDK/multiplayer lane, 2026-09-11. Branch `sdk/multiplayer-identity`. Companion:
`docs/proposals/shared-projects.md` (M1), which consumes the principal this proposal defines.

## 0. What changed, and why this document exists at all

The multiplayer plan (`.claude/plans/multiplayer-design.md`) opened with a different M0: **one identity
across AgentGrid and Wheel**, with the AgentGrid website minting short-lived audience-bound EdDSA
tokens that Wheel would accept. That premise is withdrawn. The operator's ruling:

> there's no plan for one account being usable everywhere. agentgrid local accounts stay local,
> wheel local accounts stay local, agentgrid cloud accounts stay as just agentgrid accounts. We may
> need to introduce a new auth system to wheel that supports "external" authentication, which is
> just auth that is implemented by whoever uses it.

So there is **no account unification in either direction**, and nothing in this proposal treats an
AgentGrid identity as a Wheel identity. What Wheel gains instead is a mode in which the *deployer*
brings an identity system and Wheel verifies and maps it. §8 states plainly what that leaves for the
AgentGrid↔Wheel story, including the part that does not work yet.

Two defects in the existing credential path are in scope because external auth makes them load-bearing:

1. **`aud` is never validated** (`crates/wheel-api/src/auth/claims.rs:102`, `v.validate_aud = false`).
2. **The algorithm is chosen by the attacker-supplied header**, not by the key
   (`claims.rs:41-72` branches on `header.alg`). Today that is safe only because the JWKS loader
   admits RSA keys and nothing else (`jwks.rs:113-118`) — a property of a different file.

## 1. What an external provider is — what v1 supports, and what it does not

Start from what deployers actually run, not from what is elegant.

| # | Shape | v1? | Reasoning |
|---|---|---|---|
| A | **OIDC / JWKS issuer** — Keycloak, Authentik, Dex, Zitadel, Okta, Auth0, Entra, Clerk, or an in-house signer that publishes a JWKS | **YES** | It is what every self-hosted identity system already exposes. Verification is local: no per-request outbound call, no availability coupling, no new secret held by Wheel. |
| B | **Trusted reverse proxy** that has already authenticated the user and passes an assertion — oauth2-proxy, Pomerium, `nginx auth_request`, Cloudflare Access, Tailscale serve | **YES** | This is the literal reading of "auth that is implemented by whoever uses it". A deployer who has already solved login at the edge should not have to solve it twice. It is also the most dangerous mode in the system; §5 is entirely about containing it. |
| C | **Token introspection (RFC 7662)** — Wheel POSTs an opaque token to an endpoint the deployer hosts | **NO — deferred** | Three costs the JWKS path does not pay: a synchronous outbound call on *every* authenticated request (latency, and Wheel's availability becomes the IdP's), a response cache whose invalidation semantics are a second design, and a client credential Wheel must hold for the IdP — a new secret on the credential path, which §0b treats as the thing you add last and most carefully. Shape A reaches the same answer with none of them. Revisit when a deployer asks with opaque tokens they cannot swap for JWTs. |
| D | **Static public keys in configuration** (PEM/JWK in an env var or file) | **NO — deferred** | It is shape A minus key rotation. Every OIDC implementation emits a JWKS; a deployer small enough to lack one can serve a static JSON file over HTTPS and get rotation for free. Adding a second key source now means two code paths to keep honest for no deployer we can name. |
| E | **mTLS client certificates** | **NO** | In every deployment we have seen, TLS terminates at the proxy, so this collapses into shape B with a different header. Supporting it separately would be a third code path for the same trust relationship. |
| F | **Shared-secret / HMAC tokens** | **NO, and never** | A verification key that is also a minting key means anyone who can verify can forge. That is `AUTH_DEV_SECRET`, which already exists, is already a deliberate total bypass, and is already interlocked to `WHEEL_ENV=dev` (`config.rs:152-170`). A second one wearing production clothes is strictly worse. |

**Cloudflare Access is shape A, not shape B.** It passes a *signed* JWT in
`Cf-Access-Jwt-Assertion` and publishes a JWKS. So "where the credential is read from" and "how it is
verified" are two axes, not one, and v1 configures them independently (§3). Treating Access as a
trusted-header provider would throw away a signature we could check.

## 2. Shape of the change

`AUTH_MODE` gains a third value, `external`. `local` and `jwks` are untouched; `wht_` API tokens keep
working in every mode, as they do today (`extractor.rs:66-73` runs before the mode switch).

    AUTH_MODE = local     # unchanged: users table, argon2id, HS256 sessions we issue
              | jwks      # unchanged: the deployed Clerk contract
              | external  # NEW: the deployer's identity system

`external` is a superset of `jwks` — it adds a mandatory audience, an explicit algorithm allowlist,
key-pinned algorithm selection, and local principal mapping (§4). `jwks` is retained rather than
folded in because it is what the Clerk deployment contract documents (`docs/API.md:8-27`) and
`tests/auth_verify.rs` pins; it becomes a removal candidate once nothing depends on it. Saying that
out loud is the point — two modes that differ only in rigour is a smell, and the one with less rigour
should be the one that goes.

## 3. Verification is configuration, not code

Everything the verifier does is named by an environment variable. Nothing is inferred, and nothing
has a permissive default.

| Variable | Required under `external` | Notes |
|---|---|---|
| `WHEEL_EXTERNAL_VERIFIER` | yes | `jwks` or `proxy_header`. No default. |
| `WHEEL_EXTERNAL_ISSUER` | yes | Exact `iss` to pin. Under `proxy_header` it is a synthetic stable label (§5), because the principal key needs an issuer even when no token exists. |
| `WHEEL_EXTERNAL_AUDIENCE` | yes | Comma-separated. **Exact string equality**, never a prefix. Empty refuses to boot. |
| `WHEEL_EXTERNAL_ALGS` | yes (`jwks`) | Comma-separated allowlist, e.g. `RS256,EdDSA`. No default. Symmetric algorithms are refused at parse. |
| `WHEEL_EXTERNAL_JWKS_URL` | yes (`jwks`) | `https://` in prod, non-loopback (the §5b interlock, reused). |
| `WHEEL_EXTERNAL_TOKEN_HEADER` | no | Where to read the credential. Default: the existing `x-auth-token` / `Authorization: Bearer` contract. Set to e.g. `cf-access-jwt-assertion` for shape A behind an edge. |
| `WHEEL_EXTERNAL_SUBJECT_CLAIM` | no | Default `sub`. See §4.3. |
| `WHEEL_EXTERNAL_AZP` | no | `azp` allowlist, same semantics as `CLERK_AZP`. |
| `WHEEL_EXTERNAL_MAX_TTL_SECS` | no | When set, requires `iat` and refuses `exp - iat` above it. Unset means no cap — and no cap means revocation latency equals token lifetime (§6.5). |
| `WHEEL_EXTERNAL_SOLE_AUDIENCE` | no | `1` requires our audience be the *only* one. Default off; §3.2 explains that choice. |
| `WHEEL_EXTERNAL_PROVISION` | yes | `auto` or `linked`. No default (§4.2). |
| `WHEEL_EXTERNAL_PROXY_SUBJECT_HEADER` | yes (`proxy_header`) | e.g. `x-forwarded-user`. |
| `WHEEL_EXTERNAL_PROXY_EMAIL_HEADER` | no (`proxy_header`) | Display only; never a link key (§4.4). |

Setting any `WHEEL_EXTERNAL_*` variable while `AUTH_MODE` is not `external` **refuses to boot**. A
knob that looks configured and is not read is how a deployer believes they have set an audience.

### 3.1 The algorithm comes from the key, not from the token

This is the single most important line in the proposal, and it is a change to how verification is
*structured*, not an addition to a list.

Today `verify` switches on `header.alg` and picks a verifier (`claims.rs:41-72`). The attacker writes
that header. The classic consequence — re-sign an RS256 token as HS256 using the RSA public key as
the HMAC secret — is defeated today, but by the JWKS loader refusing `oct` keys
(`jwks.rs:113-118`), which is a guarantee living in a different file from the decision it protects.
Adding a second asymmetric algorithm to that switch adds a second verifier the attacker can select.

So the order inverts:

1. `decode_header` → `kid`. A token with no `kid` is refused under `external`.
2. Resolve `kid` in the key set → `(DecodingKey, Algorithm)`, where the algorithm is derived from the
   **JWK's own key material** (`RSA` → RS256; `OKP` + `crv: Ed25519` → EdDSA), never from the token.
3. Refuse unless `header.alg == key.alg`.
4. Refuse unless `key.alg ∈ WHEEL_EXTERNAL_ALGS`.
5. Decode pinned to exactly `key.alg` — `Validation::new(one_alg)`, never a list.

An attacker can now choose only *which key* is tried, and every key carries its own algorithm. The
JWKS loader additionally refuses `oct` (as today) and refuses `OKP` whose curve is not `Ed25519` —
X25519 is a key-agreement key, and importing one as a signature key is not a mistake, it is the shape
of a downgrade.

`alg: none` has no `jsonwebtoken::Algorithm` variant, so it fails at `decode_header`. That is
incidental to a dependency, so it gets an explicit test rather than a comment.

### 3.2 Audience is mandatory, exact, and by default any-match

`aud` becomes required: a token with no `aud` is refused under `external`. Comparison is **exact
string equality** against the configured set. Never `starts_with` — `wheel:` as a prefix would accept
`wheel:some-other-deployment`, which is precisely the cross-tenant confusion this is meant to stop.

On multi-valued `aud`, we considered and **rejected** requiring a sole audience by default. A token
carrying `["https://wheel.example/api", "https://wheel.example/userinfo"]` is what Auth0 and several
others emit as a matter of course; refusing it means Wheel does not work with common IdPs out of the
box, and a security control nobody can enable is not a control. RFC 7519 §4.1.3's rule — the relying
party must find *itself* in the audience — already blocks the attack that matters: a token minted for
someone else does not name us, so it is rejected. What any-match admits is narrower and worth naming:
**another relying party listed in the same token can replay that token at us.** The IdP named that
party deliberately, so this is the deployer's trust decision; `WHEEL_EXTERNAL_SOLE_AUDIENCE=1` is the
lever for deployers who do not extend it.

### 3.3 Time

`exp` is required. `nbf` is validated when present. Leeway stays at 5 s. `WHEEL_EXTERNAL_MAX_TTL_SECS`,
when set, additionally requires `iat` and refuses `exp - iat` above the cap — so a compromised or
misconfigured IdP cannot mint a credential that outlives the operator's ability to care about it.
It is optional because an 8-hour access token is a legitimate thing for an IdP to issue, and a cap
that breaks every real deployment gets set to infinity by the first person who hits it.

## 4. Principal mapping

**A foreign subject is not a Wheel principal.** Membership (`project_members.user_id`), ownership
(`projects.owner_id`), API tokens (`api_tokens.user_id`) and attribution (`on_behalf_of`) all key off
a Wheel id that Wheel itself minted and that no external system can change.

### 4.1 The table

New in migration 0006 alongside the M1 tables (§2 of `shared-projects.md`; note 0005 is already taken
by `api_token_sessions` in both dialects — the brief's "0005" was stale):

```sql
CREATE TABLE external_identities (
    id           uuid PRIMARY KEY,
    provider     text NOT NULL,          -- operator's label, for display and logs
    issuer       text NOT NULL,          -- the `iss` that was actually verified
    subject      text NOT NULL,          -- the foreign subject claim
    user_id      uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    email        text,                   -- display only, never a link key
    created_at   timestamptz NOT NULL DEFAULT now(),
    last_seen_at timestamptz,
    disabled_at  timestamptz,
    UNIQUE (issuer, subject)
);
```

The key is **(issuer, subject)**, not (provider, subject). `provider` is a label an operator may
retitle; `issuer` is what was cryptographically asserted. Keying on the label would mean that
pointing `provider=acme` at a different issuer silently merges two populations into one set of
accounts — a configuration typo with a cross-tenant outcome. Keying on the issuer makes the same typo
fail closed, into new empty accounts.

### 4.2 Provisioning

`WHEEL_EXTERNAL_PROVISION` has no default and must be stated:

- **`auto`** — a verified token for an unknown `(issuer, subject)` creates a Wheel user and links it.
  Correct when the IdP's population *is* the intended Wheel population.
- **`linked`** — a verified token for an unknown `(issuer, subject)` is **rejected**. Someone with a
  pre-existing Wheel credential links it first, through
  `POST /v1/auth/external-identities {provider, subject, user_id}` (operator-only, sibling of
  `POST /v1/auth/users`). Correct when the IdP lets anyone sign up, where `auto` would mean anyone on
  the internet gets a Wheel account.

`linked` has no bootstrap problem: the `wht_` operator token from first boot
(`api_token::bootstrap_owner`) is the pre-existing principal that does the first linking.

### 4.3 What happens when a subject disappears, is reused, or changes issuer

The operator asked for these three explicitly. Answers, including the one that is uncomfortable:

- **Disappears** (deleted at the IdP). **Wheel does not find out.** There is no back-channel logout
  and no SCIM in v1. The `external_identities` row, the Wheel user, its projects and its memberships
  all persist; access ends only because the IdP stops issuing tokens for that subject. The operator's
  control is `DELETE /v1/auth/external-identities/{id}`, which sets `disabled_at`; verification then
  fails closed, and M1's NOTIFY revocation (§6 of `shared-projects.md`) closes any live WebSocket
  bridge rather than waiting for it to end on its own. IdP-driven deprovisioning is named as future
  work, not implied.

- **Reused** — the IdP re-assigns a retired `sub` to a different human. The new human **inherits the
  old one's Wheel account, projects and memberships, and Wheel cannot detect it.** This is the worst
  failure in the design and it is not fully fixable inside Wheel. Three things reduce it, and none is
  a guarantee: (a) OIDC Core §2 requires `sub` to be locally unique and *never reassigned*, so this is
  a provider defect, and the configuration documentation states it as a requirement of the deployer;
  (b) `WHEEL_EXTERNAL_SUBJECT_CLAIM` lets a deployer point at a better immutable identifier where the
  IdP has one (`oid` on Entra, `user_id` on several others) rather than at a mutable `sub`;
  (c) `last_seen_at` makes a dormant-then-active identity visible to an operator who looks. This
  belongs in `redteam/` as a tracked residual, not in a footnote.

- **Issuer changes** (the deployer migrates IdP, or moves a hostname). A new `(issuer, subject)` is a
  **new principal** with no projects. That is deliberate and fail-closed: the alternative —
  inheriting an account because a URL changed — is account takeover triggered by a config edit. The
  migration path is explicit re-linking through the admin route, and the docs say so rather than
  leaving an operator to discover it during a cutover.

### 4.4 Never auto-link by email

If a token carries `email`, it is stored for display and **never** used to find an existing account.
An IdP that lets a user set an unverified email address would otherwise be a one-step takeover of any
local Wheel account whose address the attacker can guess. The only automatic link is
`(issuer, subject)`; attaching an external identity to an existing local account is an explicit
operator action through the admin route.

### 4.5 The legacy wart, stated

Under `AUTH_MODE=jwks`, `projects.owner_id` holds the provider's raw `sub` text, not a Wheel uuid
(`migrations/0004_api_tokens.sql:9` records the same about `api_tokens.user_id`). So the principal
column is already two namespaces. `external` does not add a third — it always mints a local uuid —
but it does not retroactively fix `jwks`. M1 treats the principal as opaque text for exactly this
reason, and the cleanup rides with `jwks`'s removal.

## 5. `proxy_header` — the dangerous mode, and what contains it

If Wheel believes a header, then **anything that can reach Wheel directly can be anyone.** The mode is
worth supporting because it is what the operator's ruling literally describes, and it is worth
fencing hard.

**The network requirement.** Wheel must be reachable *only* through the authenticating proxy. A
trusted-peer list is not a firewall; it is the last check, not the only one.

**What is enforced, mechanically:**

1. **Boot refuses** `proxy_header` when `WHEEL_TRUSTED_PROXIES` is empty. Believing a header from
   everyone is not a configuration, it is an open door.
2. **Per request**, the **TCP peer** must be inside `WHEEL_TRUSTED_PROXIES`, or the request is 401
   whatever its headers say. Not `X-Forwarded-For` — the peer. `http::client_ip::resolve` already
   computes trust from the peer (`client_ip.rs:139-150`); it gains a `TrustedPeer` marker extension,
   set server-side, which a client cannot forge. If the `resolve` layer is absent the marker is
   absent and every request fails closed.
3. **The subject header is in the proxy's `CLIENT_ONLY` denylist** (`http/hop.rs:31-37`) so it can
   never be relayed to the host or the engine — same rule that already keeps `x-auth-token` from
   crossing the hop.
4. **CSRF.** In `proxy_header` mode the credential is *ambient*: the proxy attaches it, so a hostile
   page can make a browser issue an authenticated request. JSON routes are protected by preflight,
   but the engine proxy is `ANY` with arbitrary content types, so that is luck rather than a control.
   Therefore: **a request carrying an `Origin` header not in `CORS_ALLOWED_ORIGINS` is refused 403**
   under `proxy_header`. With the post-PR-#62 posture (`CORS_ALLOWED_ORIGINS` empty; the web app
   calls the API from its own server) that means no browser page may call the API cross-origin at
   all, which is the correct answer for ambient credentials.
5. **Boot warns, loudly and once**, naming the mode and the trusted set, so the reduced posture is in
   the first screen of logs rather than discoverable only by reading configuration.

## 6. Threat model

House format (`redteam/THREAT-MODEL.md`). Boundary **TB1 (client ↔ API)** throughout; this lands in
`redteam/` as the reviewable artefact, summarised here.

**Assets.** A1 (a session credential → account takeover) is extended: an external token, and the
`external_identities` mapping itself — whoever can write that table chooses who everyone is.

**Actors.** AN (anonymous internet), AU (authenticated other user), plus two this mode introduces:
**XI — a hostile or compromised external IdP**, which can mint any subject it likes; **XN — a network
neighbour of the deployment** that can reach Wheel without traversing the proxy.

| # | Attack | Outcome if it works | Control |
|---|---|---|---|
| 1 | Algorithm confusion: re-sign with an algorithm whose verifier treats a public key as a secret | Total forgery of any subject | §3.1 — algorithm from the key, header must agree, allowlist, single-algorithm `Validation`; `oct` and non-Ed25519 `OKP` refused at import |
| 2 | `alg: none` | Total forgery | No `Algorithm` variant; `decode_header` fails. Explicit test, because it is a dependency's property |
| 3 | Audience confusion: a token minted for another relying party (a relay, an AgentGrid host, another Wheel deployment) replayed here | That party becomes any of its users, here | §3.2 — `aud` mandatory, exact equality against a configured set |
| 4 | Prefix matching on audience | Any `wheel:*` audience accepted | Exact equality only; the test forges `wheel:evil` and asserts rejection |
| 5 | Multi-audience replay by a co-named relying party | That party can act as the user here | Accepted by default and **named**; `WHEEL_EXTERNAL_SOLE_AUDIENCE=1` (§3.2) |
| 6 | Issuer confusion between the two JWKS-backed modes | A Clerk `kid` satisfying an external token, or vice versa | Separate `JwksCache` instances; `iss` pinned per mode; boot refuses `WHEEL_EXTERNAL_ISSUER` equal to `CLERK_ISSUER` or to `PUBLIC_BASE_URL` |
| 7 | Unknown-`kid` flood → JWKS refetch amplification | DoS aimed at Wheel and at the deployer's IdP | Existing 60 s throttle (`jwks.rs:15,60-68`) applies to the new cache unchanged |
| 8 | Header-forged identity under `proxy_header` from a non-proxy peer | Total impersonation | §5.2 — TCP-peer check, fail-closed when the layer is absent |
| 9 | CSRF against ambient proxy credentials | State change as the victim | §5.4 — cross-origin `Origin` refused |
| 10 | Subject with control characters or absurd length → header injection into the actor header, log forging | Forged actor downstream; corrupt logs | Subjects validated at verification: bounded length, no control characters, restricted charset. Rejected at the boundary, not sanitised at each use |
| 11 | Auto-link by email takes over a local account | Account takeover with an unverified claim | §4.4 — no automatic email linking, ever |
| 12 | Subject reuse by the IdP | Inherited account | §4.3 — **residual**. Mitigated, not closed. Tracked in `redteam/` |
| 13 | Fail-open when external auth is half-configured | Unintended acceptance | Setting any `WHEEL_EXTERNAL_*` outside `external` mode refuses to boot; every required variable is required |
| 14 | Laundering a short external token into a long-lived credential via `POST /v1/auth/tokens` | The TTL cap becomes meaningless | An `external` credential may **not** mint `wht_` tokens. `Credential::External` is refused by the token-create route |
| 15 | Replay of a stolen token inside its lifetime | Action as the victim | Not closed: it is a bearer credential, and TTL is the control. `jti` + a replica-shared seen-set is the upgrade path, named not built |
| 16 | A stub/loopback issuer in production | Authenticates everyone as anyone (ADVERSARY 017) | Existing `reject_local_identity_provider` (`config.rs:377-410`) applied to `WHEEL_EXTERNAL_JWKS_URL` and `WHEEL_EXTERNAL_ISSUER` |

Attack 14 deserves its own line rather than a mention: it is the control that stops the entire
lifetime story from being decorative.

## 7. Testing

Every row of §6 is a test, and each is mutation-checked — the bug is restored, the test is watched
going red, the fix is restored (§0b). The verification tests use a fixture key set served by the
existing `examples/stub-issuer` harness (`docs/API.md:445-467`), extended with an Ed25519 key and an
audience parameter, so that what the tests accept and what a runnable stub emits cannot drift.

The named negative tests: RS256 token against an Ed25519 `kid`; Ed25519 token whose header claims
RS256; an algorithm outside the allowlist; a JWKS containing an `oct` key (must be skipped, not
imported); missing `aud`; wrong `aud`; `wheel:evil` against a `wheel:` prefix; `aud` array without
ours; missing `exp`; `exp - iat` over the cap; a subject with a newline; `proxy_header` from an
untrusted peer; `proxy_header` with the layer absent; a cross-origin request under `proxy_header`;
`external` attempting `POST /v1/auth/tokens`.

## 8. How AgentGrid reaches a shared Wheel project — and the gap

There is no shared account, so **AgentGrid holds a credential the Wheel deployment issued.** Three
shapes, honestly ranked:

1. **A `wht_` API token.** Works today, needs nothing from this proposal. A Wheel user creates one
   (`POST /v1/auth/tokens`), pastes it into AgentGrid, AgentGrid stores it per connection. Its
   `user_id` is a Wheel principal, so M1 membership and `on_behalf_of` attribution work unchanged.
   This is the recommended v1 path.
2. **External mode, when the deployer's IdP is also what their AgentGrid users authenticate to.**
   Then AgentGrid can obtain a token from that IdP and present it. Note carefully what this is *not*:
   it is the **deployer's** IdP, configured by the Wheel deployer. AgentGrid cloud accounts do not
   qualify unless a deployer deliberately configures AgentGrid-cloud-as-issuer, which is a per-
   deployment choice and not a product-level unification.
3. **AgentGrid cloud ↔ Wheel cloud, by default: nothing.** Naming it rather than papering over it.

### 8.1 The gap, stated plainly

The multiplayer story wanted "invite a person and they appear in both products". Without unification
the invite flow is: the Wheel owner creates an invite → the invitee gets a Wheel account on that
deployment (local or external) → accepts the invite → mints a `wht_` token → pastes it into
AgentGrid. Four steps and a copy-paste. That is the honest v1 and no amount of design here removes it.

**The sharper problem is scope, not steps.** A `wht_` token is **account-wide**: pasting one into
AgentGrid grants AgentGrid everything that Wheel account can reach, not just the shared project. For a
guest who was invited to exactly one project that is tolerable, because their account reaches exactly
one project. For an owner sharing one of twenty projects it is not: the credential they hand over is
strictly more than the access they meant to grant, and the recipient's compromise is total rather than
scoped.

The fix is **project-scoped, role-capped API tokens** — `api_tokens.project_id` plus a role ceiling,
so a token can be minted that reaches one project as `viewer` and nothing else. It is deliberately not
in this slice: it wants M1's role model to exist first, it reuses M1's revocation lineage
(`api_token::revoke`'s recursive CTE) unchanged, and it is small once both are in. **It is the
recommended next slice after M1**, and it is the piece the AgentGrid↔Wheel story actually needs. I am
naming it rather than adding it, because the alternative is designing a token scope against a role
model that is not merged yet.

## 9. What this does not do

- No account unification, in either direction. Nothing here makes an AgentGrid identity a Wheel
  identity (§0).
- No token introspection, no static keys, no mTLS, no HMAC (§1).
- No IdP-driven deprovisioning; a deleted upstream user persists in Wheel until an operator acts
  (§4.3).
- No defence against subject reuse by a non-conforming IdP (§4.3, §6 #12).
- No replay protection inside a token's lifetime (§6 #15).
- `jwks` mode keeps its missing audience validation, because changing it would break the deployed
  Clerk contract. `external` is where the rigour lives, and `jwks` is a removal candidate (§2).
