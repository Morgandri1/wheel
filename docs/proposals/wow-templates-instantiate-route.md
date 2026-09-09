# `POST /v1/projects/instantiate` — finalized route shape

Follow-up to `docs/proposals/wow-templates.md` (#26). PM's ruling: I own a single dedicated
instantiate route (create → capability patch → apply → rollback-on-failure, one atomic server
sequence), per Web's §4/§6. This doc is the finalized contract — **prep only, per PM: no
implementation branch until ADVERSARY clears the design.**

## Why a dedicated route, restated concretely

Web's §4 sequences three existing calls client-side. The failure mode that justifies owning this
server-side instead: a dropped connection between "project created" (step 2) and "board applied"
(step 4) leaves an orphaned empty project the client doesn't know to clean up, because the client
never learns the create succeeded. A single server-side route removes the gap entirely — the
sequence either fully lands or the server itself rolls back before ever replying.

## Request

```jsonc
POST /v1/projects/instantiate
Authorization: <ordinary user JWT, AuthUser — collection-level, same as POST /v1/projects>

{
  "name": "Research crew",                  // same validation as CreateProject.name
  "board": { "nodes": [...], "wires": [...] },  // EmittedBoard — byte-for-byte board/apply's shape
  "capabilities": { "http": true }           // optional; template's requires_capabilities, passed straight through
}
```

`board` is exactly `apply::EmittedBoard`. `capabilities`, when present, is exactly `Capabilities`
(today `{http: bool}`) — the same type `PATCH /v1/projects/{id}` already accepts, not a new
`requires_capabilities` type. Web's template file's `requires_capabilities` field is this value
verbatim; there is no translation step because both sides already agree on `Capabilities`'s shape.

## Sequence (one request, all server-side)

1. **Validate the board first, before creating anything** — `apply::validate(&board,
   &ExistingBoard::default(), ApplyPolicy::default())`. This is a refinement of Web's §4 (which
   validates live, after create): since a template's board only ever targets a fresh empty project,
   validation against `ExistingBoard::default()` is exactly as authoritative before creation as
   after. Doing it first means the common failure case (a malformed board slipping past the CI gate
   somehow, or a caller other than the template gallery hitting this route with a bad board) spends
   zero project-quota and starts zero sandboxes.
   - `Err(refusals)` → **`422`**, nothing created at all:
     ```jsonc
     { "applied": false, "refusals": [...], "message": "the board was refused; nothing was created" }
     ```
     Byte-identical shape to `board/apply`'s existing 422 — `readOutcome` needs no change.
2. **Create the project** — the exact body of `routes::projects::create` (quota check, insert row +
   secrets, provision, start-and-observe). Reused as a plain function the handler calls, not
   duplicated; `create`'s HTTP handler becomes a thin wrapper around the same function, or the
   function is extracted new — implementation detail, not part of this contract.
3. **If the sandbox did not come up** (status is not `Running`/`Starting` after `start_and_observe` —
   i.e. `Error`) — skip straight to rollback (step 5) with a synthetic failure step rather than
   attempting an apply call that can only fail with a confusing `engine_unreachable`.
4. **If `capabilities` was sent, `PATCH` it** (the same DB write `routes::projects::update` does for
   the capabilities field — today this is a pure DB write with no engine round trip and, per Web's
   own §6 question, I can't find a path where it refuses a valid `Capabilities` value; the
   error-handling branch below exists anyway, on the same "don't silently proceed on an unmet
   assumption" discipline as everything else in this design, not because I found a way to trigger it).
   Failure → rollback (step 5) with `{step: "capabilities", error: "..."}`.
5. **Apply the board** — `apply::execute(&plan, &ExistingBoard::default(), &HttpBoardClient::new(...))`,
   the identical call `board_apply.rs` makes today. `report.is_complete()`:
   - `true` → **`201 Created`**:
     ```jsonc
     { "applied": true, "project": { ...Project }, "report": { "created_nodes": [...], "patched_nodes": [], "created_wires": [...], "failures": [] } }
     ```
     `report`/`applied` are the same fields `readOutcome` already reads; `project` is additive and
     invisible to it — Web's existing `ApplyOutcome` rendering needs no change, per its own §5.
   - `false` (or step 3/4 failed) → rollback (step 6), reporting via the SAME `report.failures[]`
     shape used for a live apply failure. A capability-patch or dead-sandbox failure is represented
     as a synthetic entry in `failures` (`{step: "capabilities"|"sandbox", error: "..."}`, no
     `node`/`wire` field) rather than a different error shape — one failure list, one rendering path,
     whatever stage it came from.
6. **Rollback** — the same body `routes::projects::destroy` runs (orchestrator destroy, then delete
   the row only if that succeeds — never delete the row while the sandbox might still exist,
   `destroy`'s own comment explains why). Two outcomes:
   - Rollback succeeds → **`207`**:
     ```jsonc
     { "applied": false, "rolled_back": true, "report": { "created_nodes": [...], "patched_nodes": [...], "created_wires": [...], "failures": [...] } }
     ```
     `report` reflects whatever DID land before the failure (e.g. 4 of 6 nodes), even though it's all
     been deleted — useful for a bug report, harmless for the UI (which only needs `applied: false`).
   - Rollback itself fails (host unreachable, etc.) → **`207`, `rolled_back: false`**, and the project
     is NOT deleted from the API's own record (matching `destroy`'s existing behavior: never drop the
     row while the sandbox might still exist):
     ```jsonc
     { "applied": false, "rolled_back": false, "project_id": "<uuid>", "report": {...} }
     ```
     This is the one branch Web's UI needs a real (if rare) message for — "something went wrong and
     we couldn't clean up automatically" — everything else is "it didn't work, nothing was left
     behind."

## Status codes, restated as the contract Web can rely on

- `201` — full success. `project` is real and running (or at least started); go there.
- `422` — board refused, nothing exists. Identical shape to `board/apply`'s 422.
- `207` — something failed after a project was created. `rolled_back: true` is the overwhelmingly
  common case and means exactly what it says: nothing survives, same as if the request had never
  been made. `rolled_back: false` is the one case where a project id is handed back for the UI to
  say so honestly.

No other status is expected in normal operation; a `5xx` is the ordinary "the server itself broke"
case with no special handling here.

## What's reused, named explicitly (so review can check each claim)

- `apply::{validate, execute, ApplyPolicy, ExistingBoard, EmittedBoard}` — unchanged, same crate.
- `HttpBoardClient` (`board_apply.rs`) — unchanged, same engine calls `board/apply` already makes.
- Project creation body (`routes::projects::create`) and destroy body (`routes::projects::destroy`)
  — extracted to plain functions both the existing routes and this one call; no behavior change to
  either existing route.
- `ApplyReport`/`ApplyFailure` shape (`board-apply.ts`'s `report.failures[].{step,error,node,wire}`)
  — reused for capability/sandbox failures too, via synthetic `step` values, rather than inventing a
  second failure shape.
- `Capabilities` — reused verbatim as the request's `capabilities` field; no new type.

## Open items for ADVERSARY / PM, not resolved by me alone

- **Rate/quota:** `max_projects_per_user` is checked at project-creation time (step 2), same as
  today — a burst of instantiate calls is bounded the same way a burst of plain creates is. Nothing
  new to add, flagging so it's confirmed rather than assumed.
- **Auth boundary:** this route takes `AuthUser`, not `ProjectScope` (there is no project yet at
  request time) — the ownership check that matters is baked into `create` (the row is inserted with
  `owner_id = user.id()`) and into `destroy`'s reuse (rollback deletes by `id AND owner_id`,
  never a bare id). Worth ADVERSARY explicitly confirming this is equivalent to every other route's
  boundary rather than a new pattern that happens to look similar.
- **Board size vs. rollback cost:** a large legal board (near `MAX_NODES`/`MAX_WIRES`) that fails on
  its last wire pays for creating everything before finding out it must delete it all. Pre-validation
  (step 1) does not catch engine-side-only failures (a name collision is impossible here, but an
  engine check we don't mirror is not impossible in principle — `apply.rs`'s own module doc says
  this). Accepted as the same tradeoff `apply-step-constraints.md` already accepted for the general
  apply step; naming it again because a template author's board being marginal-but-legal is a more
  plausible way to hit it than an LLM's.
