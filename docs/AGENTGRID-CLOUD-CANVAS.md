# Wheel as an AgentGrid cloud canvas — client integration design

**Audience:** AgentGrid's client team, implementing against a hosted Wheel project as AgentGrid's cloud
canvas — AgentGrid acts as a puppeteer UI in front of Wheel's engine. **Status:** draft, engine-side work
in progress against this design. **Do not build a client against this yet — §2's auth section describes a
prerequisite (M0, tracked in issue #132) that has not landed. Read §6 in full before writing any code.**

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

## 2. Authentication — blocked on M0, do not build against the current state

**The auth path described here does not work today, and is unsafe to make work without the fix already
in progress.** Two problems, both in `crates/wheel-api/src/auth/claims.rs`:

1. **Algorithm.** Wheel verifies **RS256 only** (`claims.rs:49`). AgentGrid's Better Auth issuer signs
   **EdDSA (Ed25519)**. Every AgentGrid token is refused as-is — not exploitable today, just broken.
2. **Audience.** `validate_aud` is `false`, with no mandatory `aud` check. If EdDSA support were added
   without also fixing this, **any token from AgentGrid's shared issuer** — desktop, mobile, and relay
   surfaces all mint from the same issuer — would be accepted as a valid Wheel credential. That is a real
   cross-surface token confusion vulnerability, not a hypothetical one.

**The fix (M0, issue #132) is mostly already built**, parked on branch `sdk/multiplayer-external-auth`
from a closed PR, needing a rebase onto current `main` and an ADVERSARY review (it replaces the
credential path). It adds: EdDSA/Ed25519 key support keyed off the JWKS key set rather than the token
header, a **mandatory** `aud` claim via `required_spec_claims`, and `proxy_header` mode with network
containment. Once it lands:

- `AUTH_MODE=jwks` is set once, at engine boot — not a per-project or per-request toggle. (A resource's
  own owner being able to flip a security-relevant policy is treated as not a policy at all, elsewhere in
  Wheel's design; the same rule applies here.)
- The env vars will be renamed off the current `CLERK_JWKS_URL`/`CLERK_ISSUER` naming (a leftover from an
  earlier Clerk-only build) — this doc will be updated once the new names land.
- The audience configured for AgentGrid's deployment must be **Wheel-specific** (e.g. `wheel` or
  `https://api.<wheel-domain>`), never the issuer origin AgentGrid's desktop tokens already use as `aud`.
- **AgentGrid's own side:** the client must exchange the user's session for a **short-lived, Wheel-audience
  token**, not hand Wheel an existing desktop/mobile token. That exchange endpoint is AgentGrid's to
  build; the audience string is the shared contract between the two sides.

Every authenticated request (once M0 lands) carries the exchanged JWT as `x-auth-token`. The API verifies
signature, issuer, audience, and expiry per request — no separate Wheel login step. The token's `sub`
becomes the Wheel principal (`user_id` everywhere below); an optional `email` claim is carried through for
display only (masked per-viewer-tier on the roster, never used for identity).

**Do not point the JWKS URL/issuer config at anything reachable only from the Wheel API's own network**
(e.g. `localhost`, a Docker-internal name) — refused at boot as a stub-issuer misconfiguration (a stub
issuer in production authenticates everyone as anyone).

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
- **M0 (§2)** — the auth fix above. Nothing in this doc is safe to build a client against until this
  lands specifically, independent of 037/048.

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
