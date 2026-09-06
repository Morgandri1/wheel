# 034 — Engine proves its journal mode by READ-BACK, not by a write; no escalation (the store now does both)

- **Severity:** Low (fault-isolated: a project engine that can't host its mode fails at `open()` → reconcile
  logs "start failed" → that project down, grid up — NOT the grid crash-loop the host store had). Owner:
  SDK/Engine (`crates/wheel-engine/src/db/mod.rs`). This is the exact class PM is codifying ("a config override
  on a recovery path must be provable, not plausible"), still present in the engine after the host store fixed it.
- **Status:** Source review @ HEAD. The host store's rewrite (`store.rs::open_configured`/`mode_holds`) already
  embodies the rule; this is the engine's remaining instance.

## What
`set_journal_mode` (db/mod.rs:92-96): `pragma_update(journal_mode, wanted)`, then `current_journal_mode` (a
**read** pragma), and `if mode == wanted return Ok`. So the mode is PROVEN by reading the header back. But the
engine's OWN comment (and the host store's) establishes the trap: "PRAGMA journal_mode reports success on a
filesystem where the first write then fails — the shared memory is mapped on the first WRITE lock, not on the
pragma." A read-back of "wal" therefore does NOT prove WAL is usable here. The `BEGIN IMMEDIATE; COMMIT;` that
would prove it only runs in `drain_under_exclusive_lock` (140), which is reached ONLY when the read-back
MISMATCHES — so a WAL that reads-back-as-WAL-but-fails-on-write takes the early return (96) and is never
write-tested.

## Consequence
A project engine with `WHEEL_SQLITE_JOURNAL=WAL` (or the WAL default) on the `-shm`-hostile volume: pragma
succeeds, header reads "wal", `set_journal_mode` returns Ok, `configure` finishes — then the FIRST write
(`migrate`/`ensure_tables`) hits `disk I/O error … xShmMap` and `open()` errors. It is fault-isolated (host
reconcile logs it, that project stays down), so NOT the whole-grid crash-loop the host store suffered. But two
gaps vs the store: (1) the failure surfaces as a RAW SQLite error deep in `migrate`, not the store's
informative "no journal mode this filesystem can host: tried WAL, then TRUNCATE" — the informative message is
exactly what let PM diagnose the outage in one read; (2) the engine does NOT ESCALATE — it has
`drain_under_exclusive_lock` for a db ALREADY stuck in WAL, but for a fresh db whose WAL reads-back-fine it
never tries EXCLUSIVE-heap-index or falls back to TRUNCATE, so a project on the hostile volume just fails to
start instead of recovering the way the host store now does.

## Fix
Give the engine the host store's discipline (`store.rs::open_configured`/`mode_holds`): prove each mode with an
`BEGIN IMMEDIATE; COMMIT;` (a WRITE), not a read-back; try `WHEEL_SQLITE_JOURNAL` (or WAL) first, and on
failure escalate — reopen `locking_mode=EXCLUSIVE` (heap wal-index, no `-shm`), then TRUNCATE — and if none
holds, fail with an informative message naming what was tried. That makes the engine RECOVER on the hostile
volume (fall to a working mode) instead of failing to start, and makes its failure diagnosable. The two
implementations should share this logic so they cannot drift.

## Standing rule (from PM's post-mortem, recorded for red-team enforcement)
A configuration override on a SECURITY or RECOVERY path must be PROVABLE, not plausible:
1. The intended value must be tested for REACHABILITY in the actual (hostile) environment — for a journal
   mode, a write; for an SSRF/auth toggle, that the guarded branch is actually exercised — not merely that the
   setter returned Ok.
2. A wrong/unhostable value must fail LOUD with an informative message, never SILENTLY select a dead or unsafe
   branch (a lever that "looks set correctly and disables the only working branch" is the failure mode).
Good template: `WHEEL_TOOL_ALLOW_HOST` (refuses to boot in prod + WARN-logs) and the host store's
`mode_holds` (prove-by-write + loud bail). Bad template: the old force-and-trust journal pragma that caused
the 12:31 outage, and this engine read-back. Red-team reviews every new `WHEEL_*`/config override on those
paths against this.
