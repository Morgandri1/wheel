# `POST /v1/projects/instantiate` — finalized route shape

Follow-up to `docs/proposals/wow-templates.md` (#26, merged `8c845ec`). PM's ruling: I own a single
dedicated instantiate route (create → capability patch → apply → rollback-on-failure, one atomic
server sequence), per Web's §4/§6. **ADVERSARY cleared this design on 2026-09-09
(`reports/20260909T200006Z-adversary-task3-review`)**, with the two rulings folded in below
(`apply::validate()` gains name/config checks; the no-2PC orphan risk is accepted as residual, not
built around). Implementation may proceed once this doc lands.

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

   **Per ADVERSARY's design review, `validate()` itself gains two checks it does not run today**,
   before this route (or the CI gate, or the existing `board/apply` route — all three share this one
   function) can be called authoritative: for every emitted node, `wheel_core::validate_name` (or
   `validate_table_name` for a `table` node) against `node.name`, and `wheel_core::validate_config_with(&node.config, &[])`
   against its config. Today `validate()` only checks board-level structure (dupes, self-wires,
   unknown refs, size, the matrix) — a bad node name or an SSRF-denied tool `base_url` currently
   sails through pre-validation and only fails live, at the engine, on the actual create call. Adding
   these two calls closes that gap for all three callers at once, not just this route: the CI gate
   (`docs/proposals/wow-templates.md` §3) starts catching them at build time as originally promised,
   and the existing LLM-builder `board/apply` path gets the same strengthening for free. Both
   functions are pure (no engine/network dependency, `allow_hosts: &[]` — matches production, where
   the engine always runs with an empty allowlist per `validate_config_with`'s own doc), so this is
   cheap to add and belongs in the same PR as the new route, as a small first commit `apply.rs`
   change plus two new `Refusal` variants (`InvalidName`/`InvalidConfig` naming the node and the
   underlying `NameError`/`ConfigError`).
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

## Resolved by ADVERSARY's design review (`reports/20260909T200006Z-adversary-task3-review`)

**CLEAR to implement.** Everything below was an open question in the earlier draft; each is now
either fixed in this doc or accepted explicitly, not left ambiguous.

- **Extend `apply::validate()`** — the name/config checks above (step 1). Required before
  implementation, not optional hardening.
- **Auth boundary — confirmed equivalent, not just similar-looking.** ADVERSARY verified this
  structurally rather than by policy: `EmittedBoard`/`EmittedNode`/`EmittedWire` carry no id or
  project-reference field at all, wires address nodes by name within the same board only, and the
  target project comes from the route's own server-set path/context (the project this route itself
  just created) — never from anything in the request body. So there is no way for `board` content to
  name a different project; `AuthUser`-not-`ProjectScope` is safe for the same reason `create` is:
  the row is written with `owner_id = user.id()`, and `destroy`'s reuse for rollback stays
  `id AND owner_id`-scoped. Not a new pattern, just one with no `ProjectScope` to construct yet.
- **No 2PC across the Postgres project row and the target engine's sqlite — accepted as residual.**
  RULING (PM, folding in ADVERSARY's finding): an API-process crash mid-sequence (not a client
  disconnect — the atomic route already covers that; this is the process itself dying) could leave
  an orphaned partial project with no automatic sweep. Same class as `infra/prune-probe-projects.sh`
  already handles opportunistically: same-owner-only, no cross-tenant reach, no silent corruption
  (ADVERSARY separately confirmed `run_on_startup` agents are created `stopped` and only ever parked
  at boot/reconcile — nothing partially-applied can start running before a rollback completes). Do
  **not** build a pending-instantiate marker or reconciler for this pre-emptively; revisit only if
  orphans actually show up in practice.
- **Board size vs. rollback cost** — a large legal board that fails on its last wire pays for
  creating everything before finding out it must delete it all. Pre-validation does not catch
  engine-side-only failures in principle (`apply.rs`'s own module doc says so). Accepted as the same
  tradeoff `apply-step-constraints.md` already accepted for the general apply step.
- **Rate/quota** — `max_projects_per_user`, checked at project-creation time (step 2), same as
  today. Nothing new needed; not re-litigated by the review.

**Adjacent, not part of this route, noted for completeness:** ADVERSARY's review also flagged that
template-file trust today rests entirely on the deploy pipeline (operator merges to the repo) — fine
as this proposal stands (no upload path), but `apply::validate()` checks legality, not authorship,
so it would accept a user-uploaded file just as readily if that path ever opens; and that before any
self-serve gallery, an `endpoint` node's `auth` mode needs the same build-time lint
`requires_capabilities` already gets (a `mode:none` endpoint wired to an agent is finding-035-link-6's
shape, shipped as a one-click template). Both are the CI-gate/template-file side of task 3
(`wow-templates.md` §3), not this route's request/response contract — flagging here only so they
aren't lost between the two docs.
