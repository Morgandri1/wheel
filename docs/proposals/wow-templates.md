# Board templates on the website — proposal (task 3)

Web + API, 2026-09-09. Ack of `docs/wow-agent-brief.md` §3. This covers Web's half in full and
sketches the API half so the two proposals read as one; API to confirm/amend their section directly
(no web↔API wire exists on this board today — see the wire-gap note at the end — so this is routed
through PM until that's fixed).

Read the actual apply contract out of code before designing against it, not assumed:
`crates/wheel-api/src/apply.rs`, `crates/wheel-api/src/routes/board_apply.rs`,
`docs/proposals/board-apply-shape.md` (settled, live on main). The templates feature is explicitly
the sibling of the workflow-builder's apply step (`docs/proposals/workflow-builder-feature.md`) and
must reuse it, not fork it — this proposal does that literally: a template's board IS an
`EmittedBoard`.

## 1. Template file format

One file = one template. Location: `web/public/workflow_templates/<slug>.json`, `<slug>` is the
filename stem and doubles as the template's stable id (kebab-case, validated the same way a node
name is — `validateNodeName`'s charset, reused rather than reinvented).

```jsonc
{
  "title": "Research crew",
  "description": "A researcher agent with shared notes and a task queue it can read and write.",
  "requires_capabilities": { "http": false },
  "board": {
    "nodes": [
      { "name": "notes", "type": "ctx", "config": { "markdown": "# Notes\n" }, "position": { "x": 0, "y": 0 } },
      { "name": "researcher", "type": "agent",
        "config": { "harness": "claude", "system_prompt": "You research and write findings to `notes`.",
                     "run_on_startup": true },
        "position": { "x": 240, "y": 0 } },
      { "name": "creds", "type": "vault", "config": { "keys": ["SEARCH_API_KEY"] },
        "position": { "x": 240, "y": 160 } }
    ],
    "wires": [
      { "from": "notes", "to": "researcher", "type": "send" },
      { "from": "researcher", "to": "notes", "type": "write" },
      { "from": "researcher", "to": "creds", "type": "read" }
    ]
  }
}
```

**`board` is exactly the shape `POST /v1/projects/{id}/board/apply` already accepts** —
`{nodes: EmittedNode[], wires: EmittedWire[]}`, wires addressed **by name**, matching
`crates/wheel-api/src/apply.rs`'s `EmittedBoard`/`EmittedWire` (confirmed against the type
definitions and their own test fixtures, not the builder prompt's wire shape — see the wire-shape
note below, it disagrees and that's a *different* bug, not this one). Choosing this shape means a
template needs zero translation before instantiation: the file IS the request body's `board` field,
byte for byte. It is also simpler than the builder's own id-addressed per-node wires, because a
template is static — there is no synthetic id to invent and reference; the node's name (which the
file already has to be authored with) is the natural, stable reference.

`requires_capabilities` names project capabilities the board assumes (today only `http`, matching
`Capabilities`). Declared explicitly rather than inferred from "does the board contain an endpoint
node" — inference is a second definition of the same fact that can drift from the real one the
moment a new capability is added; a template author states it, the loader can still cross-check it
against the board's node types as a lint (an endpoint node with `requires_capabilities.http` unset
is suspicious and should fail the same build gate as a malformed file, see §3).

## 2. Vault-key placeholders — no special mechanism needed

A template's vault node is just `{ "type": "vault", "config": { "keys": [...] } }` — the SAME shape
a hand-placed vault node has. `VaultConfig` only ever holds key *names*; there is no field for a
value anywhere in the type, so "a template carries no live secrets" is true by construction, not by
convention someone could violate.

**Surfacing:** nothing template-specific. After instantiation the vault node exists with unset keys,
exactly like any freshly created vault — the existing vault inspector (`PUT /v1/vault/:id/:key`)
already prompts for values. The one thing worth adding is a **post-instantiate checklist**, purely
derived from the created board (no template metadata needed): "N secrets to fill in" listing
`<vault-name>/<key>` for every key on every vault node the apply just created, linking straight to
that vault's inspector panel. This is a small, generically useful summary — it would work identically
for a vault node an agent (§3e `place`) creates later, so it is not template-only surface area.

## 3. Malformed / unsupported files — fail at build time, not at runtime

Two distinct failure classes in the brief's language, and they resolve to the same mechanism:

- **Malformed** — bad JSON, missing `board`/`title`, a node with an unknown `type`, a wire with an
  unknown `type`.
- **"References a capability the target can't grant"** — read narrowly this is about
  `requires_capabilities`, but the more common real case is a board that is internally illegal: an
  unknown wire target, a self-wire, a wire the matrix forbids. Both are **fully deterministic from
  the file alone** — they don't depend on any live project's state, because a template only ever
  targets a **freshly created, empty** project (§4). That means the exact check the apply step would
  run — `crate::apply::validate(&board, &ExistingBoard::default(), ApplyPolicy::default())` — can run
  against every template file with no engine, no project, no network call.

**Proposal: a CI gate, not a runtime fallback.** A small Rust test (or binary, API's call which) in
`crates/wheel-api` iterates every `web/public/workflow_templates/*.json`, parses it as
`{title, description, requires_capabilities?, board: EmittedBoard}`, and asserts
`apply::validate(..)` returns `Ok`. Any failure — parse error or a real `Refusal` — **fails the
build**. This is the loudest failure mode available: an operator who drops a broken file gets an
unmissable CI red, not a template silently missing from the gallery and not a discovery three clicks
into "instantiate." Web's gallery code, then, only ever renders templates that already passed this
gate — it does not need to handle "malformed file" as a runtime UI state at all.

**Still not free of runtime failure**, on purpose: `validate()` against an empty board catches
everything *this codebase* currently forbids, but the matrix or the engine can still refuse
something at the live `board/apply` call (a name collision is impossible against an empty project,
but an engine-side check we don't mirror is not impossible in principle). So instantiate (§4) still
handles a real refusal from the live endpoint — it just should never see one in practice once the
CI gate is green, and when the two disagree, that disagreement is itself worth a bug report (a
template that passed the offline check but failed live means the offline check has drifted from the
engine's actual matrix).

## 4. Instantiate flow

1. User clicks a template card → names their project (prefilled from `title`, editable, validated
   the same as the existing "new project" flow).
2. `POST /v1/projects {name}` → empty project.
3. If `requires_capabilities` names anything true, `PATCH /v1/projects/:id {capabilities}` before
   applying — a template that needs `http` should not leave the user to discover their webhook 403s
   and go find the toggle themselves. If this PATCH fails, abort and roll back (step 5) rather than
   silently applying a board whose assumption (the capability) did not actually hold.
4. `POST /v1/projects/:id/board/apply {board, dry_run: false, allow_patch: false, allow_wire: false}`.
   No `dry_run` round trip first — the CI gate already proved this exact board is legal against an
   empty board, so a live preview step would only repeat that check over the network for zero new
   information. `allow_patch`/`allow_wire` are irrelevant on a project that was JUST created (nothing
   exists to patch or rewire) — left `false` because that's the safe default and there is no reason
   to ask for more.
5. **Anything other than full success rolls back**: on `422` (refused) or `207` (partial), delete the
   project (`DELETE /v1/projects/:id`) and show the user a clean failure with the refusal
   messages/failed steps, rather than a board with unwanted/incomplete nodes. This is safe in a way
   the general apply step's own doc says compensating rollback usually ISN'T (`apply-step-constraints.md`
   §3(b)): there the rollback undoes an unknown-sized partial change to a board a human might already
   be relying on. Here the "board" is a project we created ninety seconds ago solely for this attempt
   — deleting it back to nothing is a *complete* rollback, not a best-effort one, because there is
   nothing else on it that could be lost. A `422` never even created anything to roll back (the apply
   contract already guarantees this); the delete-on-422 branch exists only to clean up the empty
   shell project from step 2.
6. On success, redirect to the new project's board with the post-instantiate checklist (§2) if any
   vault keys need values.

## 5. Gallery UI

- New route, `/app/templates` (or a section of the existing new-project flow — API/PM's call on
  IA, not load-bearing for this proposal).
- Rendered from a Next.js route handler that reads `public/workflow_templates` **at request time**
  (not build-time static generation), returning parsed `{title, description, node_count, wire_count,
  warnings}` per file — client never sees raw file contents until a template is opened. Chose
  request-time over build-time SSG so the gallery reflects exactly what's deployed without needing a
  second thing to remember "did I rebuild" — since the CI gate (§3) already guarantees every file
  reaching a deploy is valid, a request-time read costs nothing in safety and removes a caching
  question. (If this turns out to be a real perf concern under load, a short `revalidate` window is
  the fix, not a switch to SSG.)
- Card shows title, description, a compact node/wire count, and the SAME capability warnings
  `parseProposal` already computes for the builder preview (codex agent, script node) — reusing that
  function's warning logic rather than a second copy, since a template is exactly the same
  `{nodes, wires}` shape it already knows how to read.
- "Use this template" opens a preview — reuse `ProposalPreview`'s rendering (node list, wire list) —
  before the name-project step, so nothing is a surprise at instantiate time. Nothing exists on the
  server until the user confirms, same invariant as the builder.
- Apply outcome rendering reuses `readOutcome`/`ApplyOutcome` (`web/src/lib/board-apply.ts`)
  unchanged — a template instantiate and a builder apply produce the same response shape by
  construction (§1), so the same result UI (plan/applied/partial/refused) applies verbatim.

## 6. API's half (my read of the brief's ask — API to confirm or correct)

- `POST /v1/projects/:id/board/apply` needs no change — it already accepts exactly this shape.
- The only new server-side surface is the CI validation gate in §3 (a test, not a route) and,
  possibly, a convenience `POST /v1/projects/:id/instantiate-template` that does steps 2-5 of §4 in
  one call instead of Web sequencing three requests — API's call whether that's worth a dedicated
  route or whether client-side sequencing (as written above) is fine. A dedicated route would make
  the rollback-on-refusal atomic-looking from Web's side (one request, one outcome) rather than a
  client having to handle "the create succeeded but the apply didn't, now I must remember to delete"
  — which is a real failure mode (a dropped connection between steps 2 and 5 leaves an orphaned empty
  project) that a server-side route sidesteps entirely by doing the whole sequence in one request.
  **I lean toward API owning this as a single route** for exactly that reason, but the validated
  create-path underneath is unchanged either way — this is a convenience wrapper, not a new
  capability.
- Capability gating: does `PATCH .../capabilities` ever refuse a capability toggle today? If never,
  step 4.3's "abort and roll back" branch is dead code today but still correct to write, since it's
  the same "don't silently proceed on an unmet assumption" discipline as everything else here.

## 7. Side finding while reading the apply contract (not blocking this proposal)

`docs/BUILDER_PROMPT.md`'s output contract tells the LLM to emit nodes with a synthetic `id` and
**per-node nested wires addressed by that id** (mirroring the engine's own `Node`/`Wire` shape).
`crates/wheel-api/src/apply.rs`'s `EmittedBoard`/`EmittedWire` — confirmed against both the struct
definitions and the module's own test fixtures — instead expects a **flat top-level `wires` array
addressed by node NAME**, and has no field to capture a per-node nested one at all. `docs/proposals/
board-apply-shape.md` (settled, live) independently confirms the flat/name-addressed shape is the
real, tested contract. Web's builder-panel.tsx sends the LLM's raw JSON straight through
unmodified (`rawBlock`, no translation step) — deliberately, per its own comment ("SDK owns the
run"), because the actual builder LLM call hasn't been wired up yet (`BUILDER_PROMPT.md` is not
`grep`-referenced anywhere in `crates/` or `web/src/`). So today this is latent, not live: once
SDK/API wire up the real builder run, if the LLM is fed the prompt as currently written, every
apply would silently create the right nodes with **zero wires**, and report success — the exact
"looks-applied-but-isn't" failure `apply.rs`'s own module doc says the whole design exists to
prevent, just arrived at from the LLM's I/O instead of the endpoint's. Flagging here because I found
it while confirming the shape my own templates rely on, not something I'm picking up — whoever wires
the real builder run should either fix the prompt to emit flat name-addressed wires (matching the
settled, tested contract) or add a translation step, and should add exactly one test that feeds the
prompt's own documented example through `apply::validate` to prove wires survive the round trip.

## 8. Open questions for API / for the joint proposal PM will route to ADVERSARY

- Single `instantiate-template` route (§6) or client-side sequencing — API's call.
- Is `/app/templates` a standalone page or folded into "new project" — IA question, not blocking the
  backend contract.
- Any naming collision handling needed for project names on instantiate? (Today `projects.create`
  takes any name; multiple instantiations of the same template presumably just get distinct project
  ids with possibly-duplicate names — confirm this is already fine, since projects are addressed by
  id everywhere, never by name.)

## 9. Board↔QA wire gap (repeat flag, still open)

`wheel connections` on this board still shows only `sdk`/`pm`/`reports`/`secrets` for `web` — no
direct wire to `api`, despite the contract describing web↔API and web↔QA as direct. Raised this
before (2026-09-06/07); still true as of this proposal. Routing this whole proposal through PM for
now; would like the wire added so "coordinate directly with API" is actually possible next time.
