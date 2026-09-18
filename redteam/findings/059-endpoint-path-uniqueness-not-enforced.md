# 059 — Endpoint `path` uniqueness is asserted in a doc comment but not enforced anywhere

- **Severity:** Medium. Not a confidentiality/authentication bypass by itself — the failure mode
  is availability/config-integrity (a working endpoint silently and permanently shadowed by a
  stale one), not unauthorized access. Raised to Medium rather than Low because the shadowing is
  *silent*: nothing on the board, in `GET /v1/board`, or at creation time tells the operator two
  endpoints now collide, and the visible symptom (the real endpoint's requests all fail) points
  everywhere except the actual cause.
- **Owner:** SDK (`crates/wheel-core/src/validate.rs::validate_endpoint`,
  `crates/wheel-engine/src/db/board.rs::create_with`/`update_with`,
  `crates/wheel-engine/src/api/ingress.rs::match_endpoint`).
- **Status:** FIXED. Investigated and run-verified as a candidate root cause for the live
  `EndpointAuth::Bearer` failure PM/Morgan reported; **ruled out** for that specific incident —
  Morgan confirmed only one `/telegram` endpoint exists on the live board. Opened as its own
  finding per PM's instruction, since the gap is real independent of that incident. sdk built the
  fix — `check_endpoint_path_unique`, `BoardError::DuplicatePath` (409, same class as `NameTaken`)
  at both `create_with` and `update_with` — PR #107, merged to `dev` as `831cb43`
  (2026-09-17T02:13:54Z). Verified by ADVERSARY at review time: mutation-tested all three call
  sites/behaviours independently (both `create_with`/`update_with` guard calls, and the by-id
  self-exclusion on update) by disabling each in turn and confirming exactly the tests meant to
  catch it fail while the negative-case tests stay green; `db::board` suite green afterward. Not
  re-run for this status update — recorded as fixed on the strength of that contemporaneous
  verification and the merge itself.

## The gap
`validate_endpoint_path`'s own doc comment states the invariant plainly:

> Endpoint paths become public URLs (`/p/<project>/<path>`) and are matched against incoming
> requests, so they must be **unambiguous** and must not escape their prefix.

But the function only checks *format* (starts with `/`, length, no `?`/`#`, no `..` segment) —
it has no access to the rest of the board and cannot check against sibling nodes. Tracing the
full create/update path confirms nothing else fills the gap:
- `board::create_with` (`db/board.rs`) enforces a DB uniqueness constraint on node **name**
  (`BoardError::NameTaken` on a `ConstraintViolation`), but there is no equivalent constraint,
  index, or application-level check on `config.path` for endpoint nodes.
- `board::update_with` has no such check either.
- `match_endpoint` (`api/ingress.rs`) resolves an incoming request by scanning
  `board::list(conn)` — which orders `ORDER BY name` — and returning the **first** node whose
  `path` and `method` match. Two endpoint nodes with the same path therefore resolve
  **deterministically** (not randomly — same outcome every request) to whichever sorts first
  alphabetically by name, silently. The loser is never routed to, ever, with no error, warning,
  or board-state flag anywhere.

## Run-verified PoC
Built and ran a test directly against `match_endpoint`/`authenticate` (not committed — this
finding is the record): two endpoint nodes with path `/hook`, method `POST`.
- `telegram` — correctly wired `read` to a vault holding the real secret.
- `aaa-old-hook` — a stale/abandoned node, also `Bearer` auth, pointing at a vault_ref that
  resolves to nothing (`NoReadableVault`).

`match_endpoint(&conn, "/hook", HttpMethod::Post)` returns `aaa-old-hook` (confirmed by
assertion — alphabetically first). Presenting the **operator's genuinely correct** secret for
`telegram` against the matched node: `authenticate()` returns `false`. Every legitimate
credential 401s, indefinitely, because the request never reaches the node it was meant for.

## Why this is worth its own finding regardless of the live incident
This was investigated as a candidate explanation for a real, reported live failure (every
presented `EndpointAuth::Bearer` credential form 401ing uniformly on the operator's `/telegram`
endpoint) — ruled out for that specific case since the live board has only one `/telegram` node.
But the underlying gap is real and would produce exactly that symptom class if it were ever hit:
a plausible sequence is an operator recreating an endpoint (delete + create, rather than editing
in place — the UI/API offer both) after getting the config wrong the first time, without
noticing or being told the old node is still there and still holds the path.

## Recommendation
Enforce the invariant the doc comment already claims:
- Add a check in `board::create_with`/`update_with` (or a `validate_endpoint_with(cfg, siblings)`
  variant, mirroring how `validate_config_with` already takes contextual data like
  `tool_allow_hosts` for other node types) that refuses a `path`+`method` combination already
  held by another endpoint node — same shape as the existing `NameTaken` refusal, same layer.
- Until that lands, `match_endpoint` should at minimum log a warning when its scan finds *more
  than one* match for a path+method (cheap: don't `return` on the first hit in a debug/audit
  path, or track a count), so a future collision is discoverable instead of silently shadowing.

## What would change my mind
If endpoint paths are intentionally allowed to collide (e.g. some future "first match wins,
useful for staged rollout" design intent), this would be a documentation correction instead — but
I found no such intent anywhere in the contract, PROTOCOL.md, or code comments; every reference
to endpoint paths treats them as identifying a single, specific handler.
