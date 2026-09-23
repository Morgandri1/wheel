# Wheel as an AgentGrid cloud canvas — client integration design

**Audience:** AgentGrid's client team, implementing against a hosted Wheel project as AgentGrid's cloud
canvas — AgentGrid acts as a puppeteer UI in front of Wheel's engine. **Status:** draft, engine-side work
in progress against this design. §2's auth prerequisite (M0, issue #132) is **built and under review, not
yet merged** — the configuration below is the real one, and it is not live on any deployment until that PR
lands on `dev`. **Read §8 in full before pointing a client at real infrastructure.**

Wheel already has the pieces a cloud canvas needs: per-project sandboxed containers, a board/wire model
for agents and their tools, a WebSocket event stream, and pluggable external auth. This doc is the API
surface AgentGrid's client calls, not a restatement of Wheel's own architecture — for the underlying
model, `docs/ARCHITECTURE.md` (node types, the wire matrix, agent lifecycle) and `docs/PROTOCOL.md` /
`docs/API.md` (full request/response shapes) are canonical; this doc only says which of those routes the
canvas flow actually uses and in what order.

## 1. Architecture, one paragraph

One Wheel **project** = one canvas. Cloud canvases each get **one dedicated container** (not a shared
process) for their engine, running the board's agents as child processes. AgentGrid's client talks to
`api.wheel.dev` — a placeholder domain, confirm the real one before shipping — never to the engine
directly: the API authenticates the caller, proves project ownership/membership, then proxies to that
project's engine. Board state (nodes, wires), agent lifecycle (start/stop/send/interrupt), and live
events all go through this one API surface.

## 2. Authentication

**Status: the fix is built, reviewed and not yet merged.** Configure against what is below; do not point a
client at a deployment until the M0 PR (issue #132) lands on `dev`. Nothing in this section is expected to
change when it does.

What was wrong on `main`, and what `AUTH_MODE=external` replaces it with:

1. **Algorithm.** `AUTH_MODE=jwks` verifies **RS256 only** and picks its verifier from the token's own `alg`
   header. AgentGrid's Better Auth issuer signs **EdDSA (Ed25519)**, so every AgentGrid token was refused —
   broken rather than exploitable. Under `external` the algorithm comes from the **key set**, never the
   header: the `kid` resolves to a key whose JWK material declares its algorithm (`RSA` → RS256, `OKP` +
   `crv: Ed25519` → EdDSA), the header must *agree* with it, the algorithm must be in the operator's
   allowlist, and decoding is pinned to that one algorithm. `oct` keys and non-Ed25519 `OKP` keys are never
   imported at all.
2. **Audience.** `jwks` sets `validate_aud = false`. If EdDSA had been added without fixing that, **any
   token from AgentGrid's shared issuer** — desktop, mobile and relay all mint from it — would have been a
   valid Wheel credential. That is cross-surface token confusion, not a hypothetical. Under `external`,
   `aud` is **mandatory** (named in `required_spec_claims`, because `jsonwebtoken` passes a token whose
   `aud` is *absent* even with validation on) and compared by **exact string equality**, never a prefix.

### The configuration

`AUTH_MODE` is set once, at API boot — not a per-project or per-request toggle. A resource's own owner being
able to flip a security-relevant policy is treated as not a policy at all elsewhere in Wheel's design, and
the same rule applies here.

```
AUTH_MODE=external
WHEEL_EXTERNAL_VERIFIER=jwks
WHEEL_EXTERNAL_ISSUER=https://<agentgrid-issuer>          # exactly the `iss` Better Auth puts in the token
WHEEL_EXTERNAL_JWKS_URL=https://<agentgrid-issuer>/api/auth/jwks
WHEEL_EXTERNAL_ALGS=EdDSA                                 # RS256 stays available for other deployers
WHEEL_EXTERNAL_AUDIENCE=https://api.<wheel-domain>        # Wheel-dedicated; see below
WHEEL_EXTERNAL_PROVISION=auto                             # or `linked`; there is no default
WHEEL_EXTERNAL_MAX_TTL_SECS=300                           # ENFORCES the 5-minute story; see below
WHEEL_EXTERNAL_SOLE_AUDIENCE=1                            # the exchange mints a single audience
```

**The last two lines are the ones that make the rest of this section true**, and they were missing
from an earlier version of this block (ADVERSARY 069). Without `WHEEL_EXTERNAL_MAX_TTL_SECS`, Wheel
accepts whatever `exp` the token carries: the 5-minute lifetime below would be AgentGrid's promise
to itself, enforced nowhere, and an issuer bug or a compromised signer could mint a 24-hour token
that Wheel would honour in full. With it, `exp - iat` above the cap is refused and `iat` becomes
mandatory, so omitting `iat` is not a way around it.

| Variable | What it does here |
|---|---|
| `WHEEL_EXTERNAL_VERIFIER=jwks` | Verify a signed token against a published key set. The other value, `proxy_header`, trusts a reverse proxy's header instead and is not what AgentGrid uses. |
| `WHEEL_EXTERNAL_ISSUER` | The exact `iss` to pin. Boot refuses it if it equals `WHEEL_JWKS_ISSUER` or `PUBLIC_BASE_URL` — two verifiers on one issuer are two token populations that can stand in for each other. |
| `WHEEL_EXTERNAL_JWKS_URL` | Where the signing keys are fetched. Cached, with key rotation on an unknown `kid` and a 60 s refetch throttle so an unknown-`kid` flood cannot pump traffic at AgentGrid's issuer. A fetched key set is trusted for the issuer's `Cache-Control: max-age` (default 10 minutes, never more than an hour), then refetched and **replaced**, so a key the issuer withdraws stops verifying within that bound. If the issuer is unreachable the held set is served for at most one further hour, then refused. |
| `WHEEL_EXTERNAL_ALGS=EdDSA` | The operator's allowlist, checked **after** the algorithm is taken from the key. A symmetric algorithm here refuses to boot by name. |
| `WHEEL_EXTERNAL_AUDIENCE` | Mandatory, exact-match. The shared contract between the two sides. |
| `WHEEL_EXTERNAL_PROVISION` | `auto` mints a Wheel account for any subject the issuer vouches for; `linked` refuses until an operator links one. No default — if AgentGrid's issuer lets anyone sign up, `auto` lets anyone into that Wheel deployment, so it is a decision to make out loud. |

The env vars are also no longer Clerk-named. `jwks` mode now reads `WHEEL_JWKS_URL`, `WHEEL_JWKS_ISSUER` and
`WHEEL_JWKS_AZP`; the `CLERK_*` spellings are deprecated aliases that still work, and setting a name and its
alias to different values refuses to boot.

### The audience is Wheel-dedicated, and never the issuer origin

**`WHEEL_EXTERNAL_AUDIENCE` must name this Wheel deployment and nothing else** — `wheel`, `wheel:prod`, or
`https://api.<wheel-domain>`. It must **never** be the issuer origin. AgentGrid's desktop tokens already
carry the issuer origin as their `aud`; configure that here and every desktop, mobile and relay token
becomes a valid Wheel cloud credential, which is the audience-confusion attack with the control switched on
and pointed the wrong way. Wheel **refuses to boot** when a configured audience equals the pinned
issuer or its origin. The one escape is `WHEEL_EXTERNAL_ALLOW_ISSUER_AUDIENCE=1`, for an issuer that mints tokens for Wheel and
nothing else; AgentGrid's issuer is not that issuer, so do not set it there.

On a multi-valued `aud`, Wheel accepts any-match by default (finding itself in the audience, per RFC 7519
§4.1.3). What that admits is narrow and named: another relying party listed in the same token can replay it
at Wheel. `WHEEL_EXTERNAL_SOLE_AUDIENCE=1` refuses a token that names anyone but us — worth setting if
AgentGrid's exchange emits a single-audience token, which it should.

### AgentGrid's side of the contract: token exchange, not a desktop token

**The client must exchange the user's session for a short-lived, Wheel-audience token.** It must not hand
Wheel a token it already had. That endpoint is AgentGrid's to build; the audience string is the contract
between the two sides.

That exchange exists on AgentGrid's branch `feat/wheel-token-exchange`:

```
POST /api/auth/wheel/token        (authenticated by the user's AgentGrid web session)
→ a 5-minute EdDSA JWT, claims: iss, sub, aud, iat, exp, jti, email
```

Five minutes is the revocation story, and it is worth being exact about who enforces which half,
because "short-lived" is the kind of claim that gets believed on both sides and implemented on
neither.

| | Enforced by | If the other side is wrong |
|---|---|---|
| the token lives 5 minutes | **AgentGrid's exchange**, when it mints `exp` | Wheel refuses anything longer *only if* `WHEEL_EXTERNAL_MAX_TTL_SECS` is set |
| Wheel will not accept a longer one | **Wheel**, `WHEEL_EXTERNAL_MAX_TTL_SECS=300` | unset means Wheel honours whatever `exp` says, up to the IdP's discretion |
| a revoked AgentGrid session stops working | **nobody, within the token's lifetime** | there is no back-channel logout and no introspection; TTL *is* the revocation mechanism |
| a stolen token cannot be replayed | **nobody** | `jti` is carried and is not checked; see below |
| a withdrawn signing key stops verifying | **Wheel**, within the JWKS cache max-age | default 10 min, hard ceiling 1 h, and the issuer's `Cache-Control` may only shorten it |

Read the third row directly: **revoking an AgentGrid session does not revoke Wheel access until the
token expires.** Five minutes is short enough that this is a reasonable trade, and it is a trade
rather than a guarantee. The operator lever that *is* immediate is
`DELETE /v1/auth/external-identities/{id}`, which fails closed on the next request.

The last row is a second, independent clock and it is the one people forget: if AgentGrid removes a
compromised signing key from its JWKS, tokens signed with it keep verifying at Wheel until the
cached key set ages out. Bounded and stated rather than unbounded and implied — the ceiling is
enforced regardless of what the issuer's `Cache-Control` advertises.

`jti` is carried and is **not** yet a replay control: Wheel does not keep a seen-set, so a stolen token is
replayable inside its lifetime, and TTL is the mitigation. A replica-shared `jti` set is named as the
upgrade path, not built.

`email` is carried through for display only — masked per-viewer-tier on the roster, and **never** used for
identity. Wheel never auto-links an external subject to an existing account by email; the only automatic
link is `(issuer, subject)`.

**An external session cannot mint `wht_` tokens.** `POST /v1/auth/tokens` returns **403** for a caller
authenticated by `external`. This is deliberate and it is what keeps the 5-minute lifetime from being
decorative: a `wht_` token is long-lived and is not revoked by AgentGrid's issuer, so trading a
five-minute credential for one would hand out indefinite access that AgentGrid can no longer take away.
Anything AgentGrid needs a durable credential for has to come from a Wheel account that logged in some
other way, deliberately — see §8.1 of `docs/proposals/external-auth.md` on project-scoped tokens, which is
the slice that actually fits this use case and is not built yet.

### Per request

Every authenticated request carries the exchanged JWT as `x-auth-token` (or `Authorization: Bearer`). The
API verifies signature, issuer, audience and expiry **per request** — there is no separate Wheel login step
and no Wheel session cookie; cookies are never credentials on this API.

The token's `sub` is **not** the Wheel principal. Wheel maps `(issuer, subject)` through an
`external_identities` table to a Wheel `users.id` that Wheel itself minted, and *that* is what
`projects.owner_id`, `project_members.user_id` and `on_behalf_of` hold. There is no account unification in
either direction: an AgentGrid identity never becomes a Wheel identity, it is mapped to one.

Two consequences worth designing for:

- **AgentGrid's issuer must never reuse a `sub`.** OIDC Core §2 requires it. If a retired `sub` is
  reassigned, the new human inherits the previous one's Wheel account, projects and memberships, and Wheel
  cannot detect it. If Better Auth exposes a better immutable identifier, `WHEEL_EXTERNAL_SUBJECT_CLAIM`
  can be pointed at it instead of `sub`.
- **Changing the issuer hostname creates new, empty principals.** A new `(issuer, subject)` pair is a new
  Wheel user with no projects. That is fail-closed on purpose — inheriting an account because a URL changed
  would be account takeover triggered by a config edit — and the migration path is explicit re-linking
  through `POST /v1/auth/external-identities`.

**Do not point the JWKS URL or issuer at anything reachable only from the Wheel API's own network** (e.g.
`localhost`, a Docker-internal name, a `.internal` hostname, or plain `http://`) — refused at boot in prod
as a stub-issuer misconfiguration. A stub issuer in production authenticates everyone as anyone.

Full operator contract: `docs/API.md`, "External auth". Design and threat model:
`docs/proposals/external-auth.md`.

## 3. Where the model credential lives

If AgentGrid supplies the model provider key (rather than each canvas owner bringing their own), **that
key must never sit in a project's vault.** Every agent in a project can read every vault the project owns
— that's finding 037's own stated constraint, and it applies even to a single-tenant board, independent
of the multi-tenancy questions in §6. A prompt-injected or misbehaving agent with vault read access has
the key.

The shape this needs, not yet built: a metering proxy AgentGrid controls, sitting between the harness and
the real model API, holding the credential itself — the harness talks to the proxy, never to a
project-vault-stored key. Design not finalized; flagging the constraint now so no interim shortcut puts a
shared provider key somewhere any project agent can read it.

## 4. Project lifecycle

```
POST   /v1/projects              {name}                    → {id, owner_id, name, status, ...}
GET    /v1/projects                                          list the caller's own projects
GET    /v1/projects/{id}
PATCH  /v1/projects/{id}         {name?, capabilities?}
DELETE /v1/projects/{id}
POST   /v1/projects/{id}/start | /stop | /restart
```

Every request needs `x-auth-token` and, for anything project-scoped, `x-project-id`. A project not owned
by (or shared with, per the tiers in §7) the caller returns **404**, never 403 — no enumeration of
projects that exist but aren't yours.

## 5. Driving the board

```
POST /v1/projects/{id}/board/apply   {board: {nodes, wires}, dry_run?, allow_patch?, allow_wire?}
```

One call creates/updates the canvas's nodes and wires, validated against the wire matrix server-side
before anything is created (never a raw import). `dry_run: true` returns the same validation without
committing — use it to preview a canvas layout before applying. See `docs/ARCHITECTURE.md` §3 for node
types and the wire matrix, and the README/`docs/SETUP.md` "Authoring a board from source" section for
the exact request/response shapes (they differ — `GET .../board`'s output is not `board/apply`'s input).

**Board positions are a seed, not the authority.** Wheel stores node positions as small integer cells; if
AgentGrid keeps its own finer-grained per-`(apiUrl, projectId)` layout client-side (as its canvas likely
does), treat Wheel's stored positions as an initial layout only — don't let an incoming `board.changed`
event fight AgentGrid's own layout state on every board mutation.

Once nodes exist, everything else — starting an agent, sending it a message, reading its log, granting
it a credential — goes through the **engine proxy**:

```
ANY  /v1/projects/{id}/engine/{*rest}    → proxied to that project's engine control plane
```

`rest` is the engine's own route space (`docs/PROTOCOL.md` §4): `agents/{id}/start`, `agents/{id}/send`,
`agents/{id}/interrupt`, `agents/{id}/log`, `vault/{id}/{key}`, `board`, etc. The API attaches the
engine's own bearer secret; AgentGrid's client never sees it. For these REST proxy calls, membership/tier
is re-checked with a fresh lookup on every request, not just once at connection time.

## 6. Spend, budgets, and metering

Cloud canvases run on API spend, so AgentGrid's client needs the routes that track and bound it — named
here explicitly rather than left for the client team to discover:

- A budget is set per agent: `{max_turns?: n, max_usd?: x}` (`AgentConfig.budget`, set via `board/apply`
  or a node patch).
- A `budget_exhausted` agent state exists and surfaces on the `node.state` event (§8) and in `GET
  /v1/projects/{id}/engine/v1/board`.
- Per-agent/per-board spend reporting: **not yet named as a discoverable route** — flagging this as
  missing rather than guessing a shape. If AgentGrid needs to read live spend (not just detect exhaustion),
  say so and we'll scope the route explicitly rather than have the client reverse-engineer one.

## 7. Live updates

```
POST /v1/projects/{id}/ws-ticket                → {ticket, expires_in: 30}   (single-use, ~30s TTL)
ANY  /v1/projects/{id}/engine/v1/events?ticket=<ticket>   (WebSocket upgrade)
```

Browsers can't set headers on a WebSocket handshake, so the ticket goes in the query string instead of
`x-auth-token`. Mint a fresh ticket immediately before opening the socket (they expire fast and are
single-use). On the connection itself, membership/tier is re-checked every 30 seconds and immediately on
any membership change — not just once at the handshake — and the socket is torn down on a lapse.

Events on the stream, all six — **implement all of them**, including the two below, before shipping a
client: a client that types the event union from a schema and then meets an undeclared frame in
production will very plausibly take its default branch (tear down and reconnect), which is actively
wrong for one of these two.

- `node.state`, `message`, `log`, `board.changed` — the obvious four.
- `wire.denied` — a capability check failed; cosmetic to omit (you just won't show denial info), but it
  is a real frame type and an unhandled-variant client will choke on it.
- `lagged` — this subscriber fell behind and events were dropped. **The socket is healthy, only behind.**
  The correct client behavior is: stay connected, refetch `GET /v1/board`, keep going. Treating this
  frame as fatal (reconnect/tear-down) is exactly the failure mode the engine's own source comments warn
  against — build the reconnect-vs-refetch branch for this one deliberately, don't let it fall through to
  a generic "unknown event" handler.

**Reconnection is not designed yet.** A canvas that stays open for hours needs: a way to fill in events
missed during a connection gap (a log sequence number to resume from, not currently exposed), and defined
behavior when a `ws-ticket` expires mid-reconnect (mint a fresh one and retry, most likely, but not
specified). Flagging as a gap rather than guessing the shape — this needs its own design pass before a
long-lived canvas client can be built reliably.

See `docs/PROTOCOL.md` for the exact shape of each event. **Not yet available:** a presence/cursor event
for multi-user live collaboration is being designed now (internal tracking only, no client-facing shape
yet) — not needed for v1 per AgentGrid's own answer in §8, single-editor is fine until it's published.

## 8. What's NOT ready yet — read before building against production

This is a **hard deadline, not background work** — confirmed directly by AgentGrid: the plan is many
cloud canvases on shared infrastructure, so the two items below gate the product itself, not just a
future roadmap item.

- **Finding 037** — every agent in a project currently runs under one shared uid per project, not one per
  agent. A compromised agent can read every other agent's credentials/data within the same project. (One
  partial mitigation has landed: the engine's own control-plane secret and vault master key are scrubbed
  from its process environment post-boot, closing the worst single carrier. Per-agent uid isolation
  itself is not built.)
- **Finding 048** — network isolation between the hosted engine and Wheel's own database/API is not
  actually deployed as designed; the segmentation the threat model assumes isn't there yet.
- **M0 (§2)** — the auth fix above. **Built and reviewed; not merged.** The configuration in §2 is the
  real one and is not expected to change, but no deployment is running it until that PR lands on `dev`, so
  a client configured against it today authenticates against nothing. Independent of 037/048.

None of the request/response shapes described in this doc are expected to change once these land — this
is a safety gate on when to point a real client at real infrastructure, not a warning that the API itself
is unstable.

## 9. Answers from AgentGrid (settled, kept here for the record)

- **Multi-user canvases: yes.** Expose Wheel's admin/prompter/guest tiers in the canvas UI — they're the
  multiplayer v1 tiers, and the masked-roster rule for guests already applies. **Caveat:**
  `redteam/findings/062` is open right now — a guest can currently read an agent's full transcript
  (system prompt, every injected context block, everything it's been told), not just the identity field
  masking covers. Don't expose the guest tier to real AgentGrid end users until that's confirmed closed.
- **Credential flow: API-key-only confirmed.** AgentGrid canvases run harness auth as `WHEEL_HARNESS_AUTH`
  API-key mode; AgentGrid will not offer OAuth login on cloud boards.
- **Presence/live-cursor: not needed for v1.** Single-editor-at-a-time is fine until the presence wire
  format (§7) is published.
