# Board templates on the website — instantiate-template

Brief: `docs/wow-agent-brief.md` §3. Owners: Web (gallery + instantiate flow) · API (this file — the
create path templates run through). Settled in git per the `board-apply-client-contract.md`
precedent: both halves are built against this, and messages have truncated in both directions.

**Status: API's half below is a proposal, not yet built. PM routes to ADVERSARY before any
implementation branch.** I'm not wired to Web directly (checked — `mcp__wheel__connections` lists
only `sdk`/`pm`), so this is written to be read and edited in place rather than argued over a
channel that keeps truncating; if Web disagrees with a section, edit it and note why.

## Headline: this needs no new engine surface, and I don't think it needs new API surface either

`POST /v1/projects` (create) and `POST /v1/projects/{id}/board/apply` (`docs/proposals/apply-step-constraints.md`,
`board-apply-client-contract.md`) already do everything the brief asks for. Instantiating a template
is:

1. `POST /v1/projects {name}` — ordinary, user-authed, subject to the existing `max_projects_per_user`
   cap (`routes/projects.rs:46`). No template-specific code.
2. `POST /v1/projects/{id}/board/apply {board: <template's board JSON>, dry_run: false}` against the
   fresh (empty) project — ordinary, `ProjectScope`-checked (same JWT-owns-project boundary every
   other project route enforces), same wire-matrix validation an LLM-emitted board gets.

Both calls already exist, are already reviewed, and are already exercised by the workflow-builder
apply step. A template is just a board whose author is a file instead of an LLM — I don't think that
difference earns new server code, and every brief constraint below is satisfied by properties this
path already has, not by anything new.

If that headline is wrong — if Web's instantiate flow needs something these two calls can't do —
that's the thing to argue about here before I write any code, not after.

## Brief constraints, mapped to what already exists

- **"a template carries no live secrets, only vault key names"** — already true by construction. A
  `vault` node's config is `{keys: string[]}` (names only); values are write-only via
  `PUT /vault/:id/:key` and never appear in a `GET`, so a template's vault node literally cannot
  contain a value — the schema has nowhere to put one. Nothing to build.
- **"instantiation runs entirely through the public project API under the user's own auth, no
  privileged shortcut"** — both calls above require the caller's own JWT; `ProjectScope` re-checks
  `project.owner_id == jwt.sub` on the apply call independently of the create call. There is no
  server-held credential in this path other than the ordinary host-secret hop every project route
  already makes to reach its own engine. Nothing to build.
- **"a template that references a capability the target can't grant is rejected at instantiation, not
  silently dropped"** — the apply step already never drops: a wire the matrix forbids is a `422`
  refusal naming the wire (`Refusal::WireNotAllowed`), and an engine-side rejection (a node type or
  config the engine refuses — e.g. the codex-harness guard that just landed) surfaces per-step in the
  `207` partial-apply report (`ApplyReport.failures[]`, step + engine's own error text), never as a
  silent skip. The `applied` field is `false` on both `422` and `207`, so a client that only checks
  `applied` cannot mistake either for success. **This is Web's half to keep true**: the instantiate UI
  must surface `refusals`/`failures` to the user rather than only checking `applied` and showing a
  generic error — the data to do that right is already in the response.
- **"a malformed file in the directory must fail loudly at load/validate, never instantiate a broken
  board"** — two layers, cheap because Web already built the first one for the workflow builder:
  - *At gallery-load time (client, no server round trip):* Web's `web/src/lib/workflow-proposal.ts`
    already has `ProposedNode`/`Proposal` types and validates a proposed board against
    `NODE_TYPES`/`isWireAllowed` before ever offering it as applicable. A template file is the same
    shape (see mapping below) minus the LLM-conversation delimiters (`START`/`END`) — reusing that
    validator (or a sibling function over the same types, skipping `extractBlock`) means a malformed
    template fails at gallery build/list time, before a user ever clicks it. I'd rather Web own this
    than fork it into a second implementation.
  - *At instantiation time (server, authoritative):* `dry_run: true` against the real, empty target
    project is a full, authoritative check against the exact matrix the engine enforces, with zero
    new server code. I'd suggest the instantiate flow always does `dry_run: true` then `dry_run: false`
    rather than skipping straight to execute — cheap (both calls hit an empty board), and it turns "the
    client-side check missed something" into a `422` instead of a partial `207`.

## Template shape — reuse, don't invent

A template file should be exactly the apply step's `EmittedBoard` (`crates/wheel-api/src/apply.rs`):
```jsonc
{ "nodes": [ { "name": "...", "type": "...", /* ...NodeConfig fields, flattened... */
               "position": { "x": 0, "y": 0 } } ],
  "wires": [ { "from": "...", "to": "...", "type": "read"|"write"|"send" } ] }
```
`EmittedNode` is `{name, position} + flatten(NodeConfig)` — structurally a `Node` minus `id` and
`wires` (wires are listed separately, by name instead of id, matching `EmittedWire`). Nothing here is
new: `NodeConfig`/`Position`/`WireType` are already exported to `docs/schema/node-config.json`,
`position.json`, `wire-type.json`, so Web's TS types for a template are the same generated types the
board editor already uses, combined the way `EmittedBoard` combines them — I don't think this needs
its own schema export, but say so if hand-deriving it is more friction than it looks from here and
I'll export `EmittedBoard`/`EmittedNode`/`EmittedWire` properly instead of leaving it implicit.

Authoring templates as this exact shape means the SAME code that renders and validates an
LLM-proposed board (the workflow builder's preview) can render and validate a template preview —
one implementation, two sources of boards, which is the whole point of "reuse, don't fork" applied
to the client side too, not just the server side.

## Open questions (for Web / for PM+ADVERSARY to weigh in on)

1. **Partial instantiate (207).** If a template partially lands (say node 4 of 6 is refused by the
   engine), what does the user see? I'd default to: show exactly what `ApplyReport` says landed and
   what didn't (never claim success), and offer "delete this project and try again" as a plain
   `DELETE /v1/projects/{id}` — no new endpoint, just a bad first draft of a template leaving a
   half-built project the user can throw away in one click rather than something bespoke to clean up.
   Alternative: auto-delete on any `207`/`422` after create, so a user never sees a half-built
   project at all. I lean toward showing it (a template author needs to see what half-landed to fix
   their file), but this is as much a UX call as an API one.
2. **Where do template files live, and who fetches them?** Brief says `public/workflow_templates`,
   read by the site — that reads as pure Web (a Next.js public directory), with no API involvement in
   listing/serving the catalogue at all. Confirming that's the intent: API's role starts at
   instantiate, not at gallery listing.
3. **Capability defaults on the fresh project.** A template with an `endpoint` node assumes the
   project's `http` capability is enabled; a fresh project's default for that — confirm it's on by
   default or the instantiate flow needs a third call (`PATCH /v1/projects/{id} {capabilities:{http:true}}`,
   which also already exists) before or alongside the apply call. Small, but worth pinning so a
   template with an endpoint doesn't silently create an unreachable one.
4. **Do we always run `dry_run` first, or only when the client wants a preview?** I lean "always" (see
   above) since it's cheap against an empty board and turns the common failure into a clean refusal;
   Web may have UX reasons to skip straight to execute for a one-click "use this template" button with
   no preview step. Either is fine from my side — say which and I'll note it as the contract.

## What I need from Web to close this

- Confirm (or correct) the template-shape mapping above, and whether `workflow-proposal.ts`'s types
  can be reused as-is (minus delimiter extraction) or need a sibling type.
- A decision on open question 1 (partial-instantiate UX) — it's the one place behavior, not just
  wiring, has to be picked before ADVERSARY can review the whole flow.
- Confirmation that nothing above requires a new API route. If something does, name the gap
  precisely (request/response shape) the way `board-apply-client-contract.md` did, and I'll build it.
