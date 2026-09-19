# Wheel as an AgentGrid cloud canvas — client integration design

**Audience:** AgentGrid's client team, implementing against a hosted Wheel project as AgentGrid's cloud
canvas. **Status:** draft, engine-side work in progress against this design — see §6 before building
anything against a real deployment.

Wheel already has the pieces a cloud canvas needs: per-project sandboxed containers, a board/wire model
for agents and their tools, a WebSocket event stream, and pluggable external auth. This doc is the API
surface AgentGrid's client calls, not a restatement of Wheel's own architecture — for the underlying
model, `docs/ARCHITECTURE.md` (node types, the wire matrix, agent lifecycle) and `docs/PROTOCOL.md` /
`docs/API.md` (full request/response shapes) are canonical; this doc only says which of those routes the
canvas flow actually uses and in what order.

## 1. Architecture, one paragraph

One Wheel **project** = one canvas. A project owns a sandboxed engine (per-project container or process,
depending on deployment) running the board's agents as child processes. AgentGrid's client talks to
`api.wheel.dev` (or a self-hosted API), never to the engine directly — the API authenticates the caller,
proves project ownership/membership, then proxies to that project's engine. Board state (nodes, wires),
agent lifecycle (start/stop/send/interrupt), and live events all go through this one API surface.

## 2. Authentication — bring-your-own via JWKS

Wheel's API supports two auth modes; AgentGrid uses **`AUTH_MODE=jwks`**, verified against a Better Auth
issuer (or any OIDC-compliant one — the verification is RS256-against-JWKS, not provider-specific code,
despite the env var names below being inherited from an earlier Clerk-only build):

- `CLERK_JWKS_URL` — AgentGrid's Better Auth JWKS endpoint.
- `CLERK_ISSUER` — the issuer string Better Auth's tokens carry as `iss`.

Every authenticated request carries the caller's Better Auth JWT as `x-auth-token`. The API verifies
signature, issuer, and expiry per request (`crates/wheel-api/src/auth/claims.rs`) — there is no separate
Wheel login step; a valid Better Auth session is a valid Wheel session. The token's `sub` becomes the
Wheel principal (`user_id` everywhere below); an optional `email` claim is carried through for display
only (masked per-viewer-tier on the roster, never used for identity).

**Do not point `CLERK_JWKS_URL`/`CLERK_ISSUER` at anything reachable only from the Wheel API's own
network** (e.g. `localhost`, a Docker-internal name) — this is refused at boot as a stub-issuer
misconfiguration (a stub issuer in production authenticates everyone as anyone).

`AUTH_MODE=jwks` is set once, at engine boot — it is not a per-project or per-request setting a project
owner can toggle. A resource's own owner being able to flip a security-relevant policy is treated as not
a policy at all elsewhere in Wheel's design, and the same rule applies here.

## 3. Project lifecycle

```
POST   /v1/projects              {name}                    → {id, owner_id, name, status, ...}
GET    /v1/projects                                          list the caller's own projects
GET    /v1/projects/{id}
PATCH  /v1/projects/{id}         {name?, capabilities?}
DELETE /v1/projects/{id}
POST   /v1/projects/{id}/start | /stop | /restart
```

Every request needs `x-auth-token` (the Better Auth JWT) and, for anything project-scoped,
`x-project-id`. A project not owned by (or shared with, per Wheel's admin/prompter/guest tiers — see §7)
the caller returns **404**, never 403 — no enumeration of projects that exist but aren't yours.

## 4. Driving the board

```
POST /v1/projects/{id}/board/apply   {board: {nodes, wires}, dry_run?, allow_patch?, allow_wire?}
```

One call creates/updates the canvas's nodes and wires, validated against the wire matrix server-side
before anything is created (never a raw import). `dry_run: true` returns the same validation without
committing — use it to preview a canvas layout before applying. See `docs/ARCHITECTURE.md` §3 for node
types and the wire matrix, and the README/`docs/SETUP.md` "Authoring a board from source" section for
the exact request/response shapes (they differ — `GET .../board`'s output is not `board/apply`'s input).

Once nodes exist, everything else — starting an agent, sending it a message, reading its log, granting
it a credential — goes through the **engine proxy**:

```
ANY  /v1/projects/{id}/engine/{*rest}    → proxied to that project's engine control plane
```

`rest` is the engine's own route space (`docs/PROTOCOL.md` §4): `agents/{id}/start`, `agents/{id}/send`,
`agents/{id}/interrupt`, `agents/{id}/log`, `vault/{id}/{key}`, `board`, etc. The API attaches the
engine's own bearer secret; AgentGrid's client never sees it. For these REST proxy calls, membership/tier
is re-checked with a fresh lookup on every request, not just once at connection time.

## 5. Live updates

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

See `docs/PROTOCOL.md` for the exact shape of each. **Not yet available:** a presence/cursor event for
multi-user live collaboration is being designed now (internal tracking only, no client-facing shape
yet) — don't build against it until a follow-up to this doc names the wire format.

## 6. What's NOT ready yet — read before building against production

Wheel's own accepted design for this integration (`docs/proposals/agent-grid-engine.md`, 2026-09-11)
explicitly gates real hosted/multi-tenant traffic on two **security findings that are still open**, not
yet closed:

- **Finding 037** — every agent in a project currently runs under one shared uid per project, not one
  per agent. A compromised agent can read every other agent's credentials/data within the same project.
- **Finding 048** — network isolation between the hosted engine and Wheel's own database/API is not
  actually deployed as designed; the segmentation the threat model assumes isn't there yet.

The API surface above is real and stable to build a client against — the request/response shapes won't
change out from under you. What's **not yet safe** is pointing that client at a hosted Wheel deployment
carrying real, untrusted-from-each-other tenants, until 037 and 048 close. If AgentGrid's rollout plan
needs real multi-tenant hosting before those land, say so explicitly and we'll treat it as a hard
deadline rather than background work — right now it's prioritized but not yet done.

## 7. Open questions for AgentGrid's team

- Multi-user canvases: does AgentGrid want role-based sharing (Wheel's admin/prompter/guest tiers, §
  `docs/ARCHITECTURE.md`) exposed in the canvas UI, or is a canvas single-user from AgentGrid's side
  with Wheel's membership model unused? **If yes** — same "tell you before you build on it" instinct as
  §6: `redteam/findings/062` is open right now and says the guest tier's read boundary isn't fully closed
  — a guest can currently read an agent's full transcript (system prompt, every injected context block,
  everything it's been told), not just the identity-masking §2 already covers. Don't expose the guest
  tier to real AgentGrid end users until that's confirmed closed.
- Credential flow: AgentGrid canvases should run harness auth as **API-key-only** (`WHEEL_HARNESS_AUTH`),
  not the OAuth-token mode reserved for self-hosted/operator use — confirm this matches AgentGrid's
  expectation before the client ever offers an OAuth-login option to end users.
- Presence/live-cursor: once designed (§5), does AgentGrid want it, or is single-editor-at-a-time
  sufficient for v1?
