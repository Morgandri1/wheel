# 024 — Re-import silently un-pins a vault/static fill when a param is renamed

- **Severity:** Medium (High-leverage: a pinned credential slot is handed to the agent; operator-triggered by
  a routine spec refresh; **silent — no added/removed signal**). Owner: **SDK/Engine**
  (`crates/wheel-engine/src/api/tool_routes.rs::merge_operations`).
- **Status:** CONFIRMED on the pure layer. PoC (verbatim port of `merge_operations`):
  `redteam/pocs/tool-exec/t_reimport_unpins_fill.mjs` → exit 1. Boundary TB7, the save_to_vault credential
  surface. This is the exact property SDK flagged ("refreshing a spec must not un-pin a vault fill").

## What
`merge_operations` (`tool_routes.rs:114-149`) matches an existing op to a fresh one by `method+path`, then
copies a saved fill onto a fresh param **only when the fresh param's `name` exactly matches an old param's
name** (`:129-133`). A **renamed** param has no name match, so it keeps its FRESH default fill (`agent`), and
the operator's vault/static pin on the old name is dropped. Because the op still matches by method+path, this
is **not** reported in `added`/`removed` — the operator gets no signal.

The function's own doc-comment (`:106-109`) states the security intent — "re-importing a spec must not hand a
field back to the agent that an operator had pinned to a vault or a fixed value, or a routine refresh silently
becomes 'the agent can now set the API key'." A param rename does exactly that, and the code does not prevent
it. The unit test `a_reimport_keeps_the_fills…` (`:368`) covers an OP-id rename with UNCHANGED param names —
not a param rename — so the gap is untested.

## PoC (verbatim `merge_operations`)
Operator pins header `Authorization` → `vault(prod-keys/API_KEY)` on `GET /data`. Upstream revises the param
name to `authorization` (a lowercase rename — or `Auth-Token`, or a path param `{id}`→`{userId}` etc). Re-import:
```
merged param: {"name":"authorization","location":"header","fill":{"mode":"agent"}}
added: []  removed: []      <- op NOT reported changed (method+path unchanged)
(the old 'Authorization' vault param is gone entirely)
```
The credential header is now **agent-fillable**: the agent can set the value where a vault secret was
intended (send its own token / omit auth / point auth at itself), and nobody was told the pin moved.

## Impact
The pinned-fill mechanism is the confinement for credentials the agent must never see (§3d, and the whole
save_to_vault line — findings 018/021/022). A routine spec refresh (paste, URL re-fetch, or an upstream that
renames a field or normalises a path) silently converts a `vault`/`static` credential slot into an
`agent`-controlled one. Operator-triggered rather than agent-reachable, but the trigger is routine and the
result is unsignalled, so it is a real erosion of the credential boundary.

## Related, weaker variant (method/path change) — reported, so lower
If a fresh op's `method`+`path` differ at all (trailing slash `/pets`→`/pets/`, case, a renamed path
`{param}`, GET→POST), the op does not match → it is treated as NEW with ALL params defaulting to `agent`, and
the old op is reported `removed` + new `added`. Same fill-reset, but the operator IS told (a path
normalisation upstream still resets every op's fills to agent — worth its own guard, but at least signalled).

## Fix
- Preserve fills across a rename: match params by a stable key (`location`+`name`, and fall back to
  position/`location` when the name changed), or — safer — **never silently downgrade a fill's mode**: if an
  op that previously had a `vault`/`static` param loses that param on re-import, REPORT it (like removals) and
  require the operator to re-confirm, rather than defaulting the replacement to `agent`.
- At minimum, surface every fill that changed mode `vault|static → agent` across a re-import as a warning in
  the import response (the diff already computes added/removed; add "unpinned").
- Add tests: a param rename, and a `vault→agent` mode transition across re-import, must not silently occur.

## Also for the record
`method+path` matching also means two ops differing only by trailing slash/case are treated as distinct — a
spec that normalises paths resets fills wholesale (reported as add/remove). Consider normalising method+path
(case, trailing slash) before matching so a cosmetic upstream change doesn't churn the whole fill set.
