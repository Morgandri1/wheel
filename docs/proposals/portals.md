# Proposal: portals — cross-project communication via existing `endpoint` + `tool` nodes

Status: **draft, for adversary + PM review**.
Author: SDK. Date: 2026-09-10. Answers `docs/wow-agent-brief.md` task 1, following the operator's 2026-09-10
de-scoping: "Portals are really just internal networking endpoints and tools being combined. That's all it's
supposed to be anyway."

## The headline finding: the data path already exists, unmodified

This is the load-bearing fact the rest of the proposal follows from, so it goes first rather than in a "what
already exists" appendix.

An `endpoint` node's public ingress route (`api/ingress.rs::handle`) and a `tool` node's HTTP executor
(`tools/execute.rs::build_request` + `send`) were both built with no assumption that the two ends are in the
same project. Concretely:

- A `tool` node's `base_url` is an ordinary string; `import.rs` parses it from whatever spec was imported and
  `execute.rs` never inspects which project it belongs to. **A tool op's `base_url` can already be set to
  another project's public ingress URL** (`https://.../p/<other-project>/<path>`) — from the engine's point of
  view that is exactly as valid a target as Slack or GitHub's API.
- The SSRF policy (`wheel_core::host_is_denied`/`ip_is_denied`, `tool.rs:262,289`) blocks loopback, RFC1918,
  link-local and `*.internal` addresses — a peer project's public ingress domain is a normal public host and is
  **already allowed**, not specially permitted or specially blocked.
- An `endpoint` node's `auth: Bearer { vault_ref }` (`node.rs:337`) already gates ingress on a secret with no
  concept of "whose project the caller is" — it does not care whether the caller is a human's curl, an internal
  cron, or another Wheel project's `tool` node hitting it. Authentication IS the boundary today, for every
  ingress caller alike.
- Every ingress delivery goes through `db::messages::enqueue` → `Message::envelope()` (`message.rs:282`), which
  calls `escape_envelope_body` (`message.rs:199`) unconditionally — there is no code path from an HTTP body to a
  child's stdin that skips it. Poison-content protection (finding 034/036) is therefore **already portal-safe**,
  because it was never endpoint-specific; it is universal to how any message becomes an envelope.

**So the "in" side of a portal is an ordinary `endpoint` node, and the "out" side is an ordinary `tool` node,
today, with zero engine changes.** An operator who wants this right now can build it by hand: create an
`endpoint` in project B with `auth: Bearer{vault_ref: "creds/PORTAL_TOKEN"}`, put a value at `creds/
PORTAL_TOKEN`, then in project A create a `tool` node whose op has `base_url` pointing at B's `/p/<B>/<path>`
and an `Authorization` header filled from a vault holding the same value. That is a portal. Nothing in this
proposal is required to make that work.

## What is actually missing, then

Given the above, task 1's real gap is not a data-path capability — it's everything the brief's own
"non-negotiable constraints" ask for that a hand-built pairing does NOT give you:

1. **Visibility**: two projects quietly pointed at each other via a `tool` node's `base_url` string look
   identical to "this project calls Stripe." Nothing on either board says "this is a portal to project X,"
   nothing lists it, nothing lets an operator find and revoke it without knowing to go look at that one tool
   node's config.
2. **A real grant lifecycle**: today the "grant" is "an operator with access to both projects manually copies a
   secret between two vault PUTs." That is explicit and revocable (delete the vault key on either side kills it
   instantly — `DELETE /v1/vault/:id/:key` already exists, no new revoke path needed), but it is not a *feature*
   — there's no record of it, no UI for it, no way to grant across two DIFFERENT operators' projects without an
   out-of-band channel to exchange the secret.
3. **Framing/discoverability for two different owners**: everything above assumes one operator holds both
   projects (or two operators who can already message each other outside Wheel). A cross-owner "project X wants
   to portal into project Y, Y's owner approves" flow does not exist at all.

None of these three are engine (wheel-core/wheel-engine) problems. All three are **project-level, cross-project
concerns that belong where project ownership already lives — the API**, per `wheel-api/src/auth/extractor.rs`'s
`ProjectScope` (proof that an authenticated user owns a specific project) and Postgres (`projects`,
`project_secrets`). The engine holds no concept of "which human owns this project" and no channel to another
project's engine at all (confirmed: `wheel-host`'s docker backend deliberately binds no ports reachable from the
host network — `sandbox/docker.rs:133` — so any inter-project traffic has to leave to the public internet and
come back in through the peer's public ingress, exactly like a portal built from `tool`+`endpoint` already does;
there is no engine-to-engine shortcut to build one even if this proposal wanted to).

## Design

### Not a new node type, not a wire attribute — an API-owned pairing record

Answering the brief's first open question directly: **neither**. A portal is a row in a new API-owned table
(Postgres, alongside `projects`), not a wheel-core schema addition:

```
project_portals (
  id, from_project_id, from_tool_node_id, to_project_id, to_endpoint_node_id,
  token_vault_ref_from, token_vault_ref_to, created_by_user_id, created_at, revoked_at
)
```

This row is metadata ABOUT an existing tool node and an existing endpoint node — it does not change what either
node is or how the wire matrix treats it (both keep behaving as ordinary intra-project nodes; `add_wire`/
`check_wire` need no change, confirmed structurally: both always look up `from`/`to` in the same sqlite
`Connection`, board.rs:417-421, so there is no cross-project edge to add wire-matrix support for). Its only jobs
are (a) give the pairing a place to be listed/shown/revoked as its own first-class thing, and (b) let the API
mint and distribute the shared credential on the operator's behalf instead of manual copy-paste.

### Peer addressing: project id for humans, bearer token for the actual gate

Second open question: **project id names the pairing for the UI; the bearer token is the entire access control
at request time.** The engine has no idea what a "peer project" is and does not need to — from the receiving
endpoint's perspective this is exactly the same `EndpointAuth::Bearer` check every other authenticated ingress
caller goes through. Nothing new is added to the wire-gated authentication path; the portal concept lives
entirely one layer up.

### Grant lifecycle

**v1, same-owner (the operator's own case, and the common early case — ship this):**
1. User A (owns project A) initiates: "portal project A's tool `<node>` to project B's endpoint `<node>`" —
   requires A to also own B (checked via the same `ProjectScope` ownership check every project route already
   uses, run twice, once per project).
2. API mints a random token, writes it into project B's vault via `PUT /v1/vault/:id/:key` (API already reaches
   this — it's the same engine control-plane route the vault UI itself calls) at a key the endpoint's `auth`
   already references, and into project A's vault at a key the tool op's `Authorization` fill already
   references. Both are calls to functionality that exists today; nothing new is added to either engine's vault
   route.
3. `project_portals` row created, surfaced on both projects' boards as a distinct, labeled card (Web's half) —
   this is the "real, visible, revocable" requirement satisfied structurally, not by convention.
4. Revoke (either side, either owner if cross-owner later): `DELETE /v1/vault/:id/:key` on the token, mark the
   row `revoked_at`. The portal is dead the instant the shared secret is gone — no new revoke mechanism, reusing
   what vault deletion already does.

**v2, cross-owner (explicitly deferred, not in this proposal's scope):** an invite/accept flow between two
different users' projects. Real design work (how does B's owner discover/approve a request from a stranger's
project A, what does the invite link/code look like, does it expire) that deserves its own review rather than
riding in on the de-scoped version of task 1. Nothing in the same-owner design below blocks adding this later —
`project_portals.created_by_user_id` and a `pending`/`accepted` state on the row is enough headroom.

### Delivery guarantees

Fourth open question: **inherited unchanged from ingress today, nothing new.** A portal hit is a normal ingress
request: the moment the engine answers `202 {"accepted":true,"queued":N}`, the message is a durable sqlite row
(`queued` state) — that already survives a deploy/drain because it is a row, not in-memory state. "At-least-once
from the caller's perspective" is already true of every ingress caller (a caller that retries after a timed-out
response may enqueue twice) and a portal caller is not a special case of this — the calling `tool` node gets
exactly the same `{status, headers, body}` outcome any tool call gets and can retry exactly as any other tool
call integration already might.

### Constraint checklist (the brief's "non-negotiable" list)

- **No ambient cross-project access, explicit + revocable grant**: satisfied — the bearer token IS the grant,
  minted once per pairing, dead the moment either vault key is deleted. Nothing is reachable without it; nothing
  about project A or B's existence alone grants access to the other.
- **Wire matrix still governs each side**: satisfied, and unmodified — the `endpoint`'s own `send`/`write` wires
  decide who INSIDE project B sees the delivered message; the `tool`'s own `vault, read` wire decides what can
  resolve the credential fill inside project A. A portal adds no new wire-matrix cell and needs none.
- **Poison-content passes the same escaping**: satisfied structurally, and was never portal-specific — see the
  headline finding above.

## Open questions this proposal is taking a position on (not leaving open)

- New node type or wire attribute? — Neither; an API-level pairing record (settled above).
- Peer addressing? — Project id for humans, bearer token for the actual gate (settled above).
- Delivery guarantees? — Inherited unchanged from today's ingress semantics (settled above).
- Grant lifecycle? — Same-owner mint-and-distribute in v1; cross-owner invite/accept explicitly deferred
  (settled above).

## Non-goals

- No engine-to-engine channel. Confirmed none exists and none is needed; all inter-project traffic already goes
  out to the public internet and back through the peer's own public ingress, same as any other HTTP integration.
- No wheel-core/wheel-engine schema change. `NodeType`, `NodeConfig`, and the wire matrix are untouched.
- No cross-owner pairing in v1 (deferred, see above).

## Owners

API: `project_portals` table, the pairing/mint/revoke routes, `ProjectScope`-gated ownership checks, calling
each project's existing engine `PUT /v1/vault/:id/:key` on the operator's behalf. Web: the portal card UI on
both projects' boards (create, view, revoke), pointing at existing tool/endpoint nodes rather than a new node
palette entry. SDK: none identified as new engine work — this proposal's SDK deliverable is the analysis above
and standing by during adversary review for anything that surfaces a real gap in the existing ingress/tool/vault
mechanisms this leans on.
