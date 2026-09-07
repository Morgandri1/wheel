# 049 — Board apply (`apply.rs`): name-collision is a SILENT PATCH of existing nodes; validate uses emitted type; no board-size cap

- **Severity:** Medium (integrity/privilege via untrusted-board import; the wire-escalation half is contained by
  the engine, see below). Owner: SDK/Engine (`crates/wheel-api/src/apply.rs`) + whoever wires the route.
  Reviewed BEFORE it has a URL, at SDK's request — the right time. Boundary TB1 (import) → board state.
- **Status:** Source review of `apply.rs` (validate/execute) + the engine backstops it relies on. Not yet
  route-wired; no live exploit. Three issues, ranked.

## 1. Name-collision → SILENT PATCH of an existing node (the sharp one)
`validate` (apply.rs:192-198) sends any emitted node whose name matches an existing one to `patch_nodes`, and
`execute` (apply.rs:288-309) merge-patches its config into the existing node BY ID. The module's own header
says the input is "JSON an LLM produced" — untrusted — and §3e adds a TEMPLATE gallery (boards authored by
others). So "apply this board" silently becomes "apply AND MODIFY whichever existing nodes it happens to name."
A same-TYPE collision merges cleanly (the engine accepts it — the type check below only blocks a type CHANGE),
so an imported template or LLM-emitted board that names an existing `agent` can overwrite security-relevant
config on it:
- `system_prompt` → a permanent prompt injection of that agent (pm, say);
- `workspaces` → clone an attacker's repo into the agent's cwd on next start;
- `budget` / `idle_timeout_secs` / `run_on_startup` → change how/whether it runs.
Merge-patch (RFC 7386) is right for an intentional edit; the problem is that a collision is treated as an
intentional edit with no signal. Nothing tells the importer "this will MODIFY pm," and nothing distinguishes an
untrusted board source (template/LLM) from the owner deliberately editing.
**Fix:** import defaults to CREATE-ONLY — a name collision is a `Refusal` (like the within-board duplicate
already is), not a silent patch. Patching existing nodes is a separate, explicit, owner-confirmed mode, and the
plan/route MUST surface "this import will modify these existing nodes: [list]" before applying. Consider
refusing to patch sensitive fields (`system_prompt`, `workspaces`, vault keys) from an import at all.

## 2. `validate` uses the EMITTED type for a colliding name (SDK's flag — real, but CONTAINED)
`validate` builds its type map from emitted nodes first and `or_insert`s existing types (apply.rs:140-153), so a
name colliding with an existing node of a DIFFERENT type is wire-checked as the EMITTED type. Then that node is
patched (which the engine refuses as a type change) while its wires were validated under the wrong type. I
traced whether this ESCALATES: it does not. The engine backstops both halves —
- `board::add_wire` (db/board.rs) looks up BOTH endpoints' REAL types by id and runs `check_wire` on them, so a
  wire validated by apply.rs as `ctx→agent` but where "ctx" is really an agent is refused at creation;
- `patch_node` (api/board_routes.rs) re-tags the merged config with the EXISTING node's type and re-parses ("a
  PATCH may never change what kind of node this is"), so a mismatched-type merge 400s, never corrupts or
  changes type.
So no escalation and no corruption — BUT apply.rs's stated guarantee ("**Nothing is created until everything is
checked** … nothing ILLEGAL was attempted — validate ran first") is FALSE in the collision case: `validate`
passes a board whose wires are illegal for the real types, `execute` attempts them, and the engine refuses →
a confusing PARTIAL apply (some nodes/wires land, the colliding ones fail) that the docs say cannot happen.
**Fix:** resolve a colliding name to its EXISTING type (existing wins over emitted), OR — cleaner, and it
composes with #1 — refuse a collision outright when the emitted type differs from the existing type. Either
makes `validate` agree with what the engine will do and restores the "nothing illegal attempted" guarantee.

## 3. Board/project size bounds — CORRECTED (there is NO per-project node cap at any layer)
My first draft said "the per-project node cap is engine-side only … the cap rejects after N." That implied an
engine backstop that DOES NOT EXIST. SDK grepped and I re-checked: nothing counts nodes anywhere in the engine,
and §3e's default-50 per-project cap is UNIMPLEMENTED (like `wheel place`). A doc specifying a cap is not the
cap existing — the same false-clean/quick-check error this campaign keeps catching, here in my own finding.
Owned. Note apply.rs:17's own comment repeats it ("does not cover engine-side failures (name collision,
per-project caps)") — SDK should fix that comment too, so it does not imply a per-project cap that is not there.

Accurate statement:
- The ONLY size bound that exists is the API's board-apply REQUEST cap — `MAX_NODES=200` / `MAX_WIRES=1000`
  (apply.rs:167-184, checked at :179; commit 2455871). It bounds ONE apply REQUEST, not the board and not the
  project total. Good and correct as far as it goes.
- There is NO per-project node cap at ANY layer today (engine: none; §3e's 50: unimplemented). So repeated
  applies (each ≤200 nodes) grow the board without limit, and nothing counts total nodes — the DoS is the
  unbounded TOTAL, not a single oversized request (which the API cap now catches).
**Fix:** land the §3e per-project node cap engine-side (SDK, queued post-wake) — it is the only place a total
bound can be enforced across many applies and other node-creating paths (`wheel place`, the UI). Until it
exists, the request-cap is a per-call bound only; do not describe or rely on it as a per-project bound.

## Note (what is SOUND, so the fix stays scoped)
The engine is the real gate and holds: wire matrix re-validated on real types at creation; type immutable across
PATCH; merge-not-replace (no silent field erasure). `validate`'s within-board duplicate-name refusal and
unknown-node/self-wire checks are correct. The issues are all at the apply/import LAYER: it is optimistic where
the engine is strict (#2), and it treats a collision as consent (#1). Fixing #1 (create-only default) largely
subsumes #2, because a type-mismatched collision then never reaches the wire check as the wrong type.
