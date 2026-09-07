# Apply step — two constraints found before building

API, 2026-09-07. Ack of `docs/proposals/workflow-builder-feature.md` §"The apply step" (328cccd).
In git because the assigning message arrived beheaded; I recovered the scope from the doc, which is
the git-first discipline working as intended.

## 1. The plan's premise holds — verified, not assumed

The plan says "merge-PATCH is live, RFC-7386, so partial config is safe". It is, and it is tested.
I checked rather than took it, because **I proved the opposite this morning** (`dogfood-api-gaps.md`
§3: PATCH replaced the config wholesale and silently dropped `workspaces`, `budget`,
`idle_timeout_secs`). SDK has since landed the merge. Current state on main:

- `patch_node` merges via RFC 7386 and validates the MERGED RESULT before storing, so an invalid
  merge is a 400 rather than a stored-and-broken config.
- Four in-module tests, all green: a patch touching one field leaves the others alone; an explicit
  `null` removes a field; a nested object merges instead of replacing siblings; **an array is
  replaced wholesale, not merged element-wise**.

So the improve-diff can send partial config safely. My earlier finding is closed by their fix, not
by anything of mine.

## 2. Arrays are replaced wholesale — the diff must read before it writes

RFC 7386 has no element-wise array merge, and the engine's test pins that behaviour deliberately.
`workspaces` is a `Vec<Workspace>`, so:

- omitting `workspaces` preserves it;
- sending `workspaces` REPLACES the whole list.

There is no "append one workspace" patch. So the improve-diff cannot emit blind deltas for array
fields — it must read the current config and send the full intended array. Same applies to any
future array field. Not a defect; a property the diff has to be written around, and cheaper to know
now than to find via a lost workspace.

## 3. All-or-nothing is NOT achievable at this layer today

The plan asks for "all-or-nothing where possible, or report exactly what landed and what failed".
Stating plainly which of those we get, so nobody reads the stronger one into it:

Realising a board is N separate calls to the engine — `POST /v1/nodes` per node, `POST /v1/wires`
per wire. There is no batch route and no transaction spanning them (confirmed against the engine's
route table: both are single-item endpoints). So a refusal on wire 7 of 9 leaves 6 wires and every
node already created. Options:

- **(a) Engine-side batch route** — `POST /v1/board/apply` taking nodes+wires, applied in one sqlite
  transaction. Real atomicity. SDK's crate, SDK's call, and not something I can do from the API.
- **(b) Compensating rollback** — the apply step deletes what it created on failure. Best-effort
  only: the rollback itself can fail, and then the report has to describe a partially-rolled-back
  board, which is worse to explain than a partial apply.
- **(c) Validate-then-apply** — check every wire against the default-DENY matrix (wheel-core's
  `check_wire` is `pub`, so the apply step can pre-validate without a round trip) and refuse the
  WHOLE board before creating anything if any wire is illegal.

**(c) removes the common case cheaply** and is entirely in my layer: the LLM emitting an illegal
wire is the expected failure, and pre-validation turns it into a refusal before a single node
exists. It does not cover engine-side failures (name collision, cap exceeded, node-type validation),
so (a) is still the only true atomicity.

**I intend (c) + precise reporting, and to ask SDK whether (a) is in scope** rather than build (b),
whose failure mode is a board nobody can describe. If SDK adds the batch route the apply step
collapses into one call and gets real atomicity; until then the honest contract is "nothing illegal
was attempted, and here is exactly what landed".

The success-shape invariant is unaffected either way: a partial apply is never reported as success.
