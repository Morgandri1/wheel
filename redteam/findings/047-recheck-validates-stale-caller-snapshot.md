# 047 — The query/tool_call capability-window re-checks are INEFFECTIVE: `require()` validates the caller's wire SNAPSHOT, not live wires

- **Severity:** High (capability boundary — "the premise the entire product rests on" — and it silently defeats
  TWO shipped fixes, so the window everyone believes is closed is open). Owner: SDK/Engine. Boundary TB4
  (agent → table/tool). Supersedes the fix half of 046: SDK ADDED the tool_call re-check (698cbe8), but it does
  not work, for the same reason the query re-check (fe8bdcf) does not.
- **Status:** CONFIRMED by RUN against a build that provably contains both re-checks, AND by source. This is the
  answer to SDK's standing invitation ("the answer should match that description, and if it does not I want to
  know"): it does NOT match. The re-check reflects request-START, not the moment of disclosure.

## The mechanism (source)
`Caller::authenticate` (caps.rs:74-84) resolves the token and calls `board::get(conn, id)` ONCE, storing the
whole `Node` — INCLUDING its `wires` — in `self.node`. That is a snapshot taken at request start.
`Caller::require` (caps.rs:91-112) reads the TARGET live (`board::get_by_name`, :97) but checks the wire with
`self.node.has_wire(...)` (:104) — the caller's SNAPSHOTTED wires, never re-read from `conn`.

Both "capability window" fixes re-run `me.require(&conn, ...)` with the SAME request-start `me`:
- query (fe8bdcf, "close the query capability window"): `require` before the read, `require` again after
  `tables::query`, before disclosure — same `me`.
- tool_call (698cbe8, SDK's 046 fix): `require` before the call, and the re-check before disclosing — same `me`.
Because `require` checks `self.node.wires` (fixed at authenticate), the SECOND `require` re-validates the SAME
snapshot as the first. A wire revoked mid-request changes the DB, not the snapshot, so the re-check cannot see
it. The re-check is a no-op against its own purpose.

## Proven by run (recorded against the engine it measured, per SDK's /healthz build)
PoC: `redteam/pocs/wire-race/t_wire_race.py`. Engine build **d47c4a4** (from `/healthz`), which
`git merge-base --is-ancestor` confirms contains BOTH `fe8bdcf` and `698cbe8` — so the re-checks were present
in the code under test.
- **revocation IS reflected across requests (PASS):** delete wire → a NEW `wheel query` gets exit 3. (A fresh
  request re-authenticates → fresh snapshot → sees the deletion.) So live wires ARE seen — but only at
  authenticate, i.e. per request, not within one.
- **mid-request re-check does NOT withhold (THE FINDING):** start a slow `query` (120-row 4-way self-join,
  1.45s), delete BOTH the agent→table wires at ~0.25s (DELETE returns 204, ~1.2s before the query finishes),
  and the query returns `{"n":207360000}` with exit 0 — rows DISCLOSED, against a capability revoked long
  before disclosure. The re-check ran (it is in d47c4a4) and passed against the stale snapshot.
- **single-writer handlers are NOT affected (PASS):** hammering `read` while flapping the wire produced only
  clean exit codes, never a torn/500 state — the lock-holding handlers (read/write/rm) hold the writer across
  check AND action, so there is no mid-request window for them. The finding is specific to the two handlers
  that RELEASE the lock (query, tool_call) and rely on the ineffective re-check.

## Impact
The revocation TOCTOU the re-checks were added to close is OPEN. An operator who revokes an agent's wire while
a call is in flight does not stop that call: `query` discloses the rows it read, and `tool_call` performs the
external request (with the tool's vault credentials) AND discloses the response — both against a capability
that no longer holds at disclosure. Worse than "not fixed": the fixes make the code READ as if the window is
closed (a re-check is right there), so a reviewer, a test that only checks "fresh request after revoke = deny",
and SDK's own description all believe it holds. It does not.

## Fix (SDK)
The re-check must read the caller's LIVE wires, not the authenticate-time snapshot. Two ways:
1. Re-AUTHENTICATE before the re-check: call `caller(&s, &headers)` (or re-`board::get` the caller node) under
   the re-check lock to get a fresh `Caller`, then `require` on THAT. This makes the re-check reflect the
   moment of disclosure, as the guarantee claims.
2. Or make `require` re-read the caller node's wires from `conn` (e.g. `board::get(conn, self.node.id)`) rather
   than trusting `self.node.wires`, so EVERY `require` is live — which also removes the footgun of a long-lived
   `Caller` whose authority silently goes stale.
Option 2 is the more robust: it makes the one capability entry point live by construction, so a future
lock-releasing handler cannot reintroduce this. Add a test that revokes the wire BETWEEN two `require` calls on
the same `Caller` and asserts the second denies — the property no current test exercises (they all re-check
via a fresh request, which refreshes the snapshot and hides this).

## Note
Caught only by revoking MID-request and observing disclosure — a fresh-request-after-revoke test passes (the
snapshot refreshes per request) and hides it. Recorded against build d47c4a4 rather than a presumed tag,
exactly the reproducibility SDK wired `/healthz build` for; current `origin/main` caps.rs has the same
`self.node.has_wire` snapshot check, so main is affected, not only this build.
