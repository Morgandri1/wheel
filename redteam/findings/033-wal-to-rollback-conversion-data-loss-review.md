# 033 — DATA-LOSS review: WAL→rollback conversion at boot (engine + host store) on a flaky volume

- **Type:** Data-integrity/durability review (PM requested, highest-value). Owner: SDK/Engine
  (`crates/wheel-engine/src/db/mod.rs`) + API/host (`crates/wheel-host/src/store.rs`, still being written).
  Boundary: durability of `/data/wheel.db` (per project) and `/data/host.db` on Railway's I/O-flaky bind mount.
- **Verdict:** The two scariest paths are SOUND by design — Q1/Q2 (crash mid-conversion) are crash-atomic in
  SQLite, and Q3 (two engines racing one file) is DEFEATED by `reap_leftovers`. Two real fixes: **F1 (engine)
  synchronous/busy set AFTER the conversion**, **F2 (host store) missing the EXCLUSIVE escape hatch + explicit
  synchronous**. Neither is corrupting-by-default, but both remove a dependence on luck on a volume that has
  already thrown I/O errors.

## Q1 — crash MIDWAY through the conversion: what is on disk? → recoverable
The conversion is a single `PRAGMA journal_mode=TRUNCATE` on a WAL db (`drain_under_exclusive_lock`,
db/mod.rs:130-142) — NOT manual `-wal`/`-shm` file surgery. SQLite checkpoints the WAL fully into the main db
and fsyncs main BEFORE flipping the file header to rollback mode, so a crash leaves ONE of two good states:
(a) header still WAL + intact `-wal` → re-checkpointed on next open; (b) fully-converted rollback, `-wal`
stale/ignored. There is no on-disk state that drops a committed transaction — **modulo F1** (the checkpoint's
sync must be FULL on this volume).

## Q2 — is a half-drained WAL recoverable? → yes
Mid-checkpoint crash with a still-WAL header → next open is WAL (`journal_mode()` default) and SQLite replays
the WAL (that is what the WAL is for). If the checkpoint's fsync FAILS on the flaky volume, SQLite returns an
error; the code's read-back guard catches it — `set_journal_mode` re-reads the mode and
`ensure!(settled == wanted)` (db/mod.rs:111-115), so a failed conversion makes `open()` ERROR rather than
proceed on a lie. The host then fault-isolates that project (reconcile is log-and-continue, finding 032); the
db is left consistent (WAL intact) and re-converts on a later healthy boot. Good — the read-back is the real
gate, not the pragma's return value (the code says so, and it is right).

## Q3 — can two engines race the same file on a host restart? → DEFEATED
Children are spawned `kill_on_drop(false)` so a host crash does NOT take tenants down — which means an old
engine can OUTLIVE the host and still hold `/data/wheel.db`. `start()` handles this: BEFORE spawning the new
engine (process.rs:389) it calls `reap_leftovers(id, uid)` (356), which finds every process in the project's
uid range via `/proc` (`leftover_pids`), SIGTERMs them (graceful — lets a live orphan flush sqlite), waits
`reap_grace_secs`, then SIGKILLs survivors. Comment: "leaving it would mean a second process for this
project's nodes the moment the new engine starts." So the old engine's file lock is released before the new
one opens+converts — no two-writer race. (SQLite's POSIX locking would prevent corruption anyway; the reap
prevents the conversion from failing on contention.) Residual: a SIGKILL of an orphan mid-conversion → a
crash mid-conversion → recoverable per Q1. Credit the host author.

## Q4 — is synchronous=FULL reached before the first write? → NO (F1)
`configure()` (db/mod.rs:144-160): `set_journal_mode` (the conversion, a large checkpoint write) runs at
**line 147**; `synchronous` is set at **158** and `busy_timeout` at **159** — AFTER the conversion. So the
riskiest write on the flaky volume runs at the connection's DEFAULT synchronous and with `busy_timeout=0`
(no contention tolerance for the EXCLUSIVE-lock conversion). It is *probably* not corrupting — SQLite's
compile-time default synchronous is FULL(2), and a WAL checkpoint fsyncs the main db regardless of NORMAL — but
on a volume that has already thrown I/O errors the code must NOT depend on the default.
**F1 fix:** set `synchronous=FULL` and `busy_timeout` BEFORE `set_journal_mode`, so the conversion checkpoint
is explicitly durable and can wait out transient contention (e.g. the `tables::query` second connection, or a
just-reaped lock not yet released). One-line reorder in `configure()`.

## F2 — the host store (`store.rs`) lacks the engine's EXCLUSIVE escape hatch AND explicit synchronous
`store.rs::set_journal_mode` (35-50) does a PLAIN `pragma journal_mode=TRUNCATE` fallback when WAL "doesn't
hold." But the engine's whole reason for `drain_under_exclusive_lock` is a db ALREADY stuck in WAL on this
volume: leaving WAL requires a checkpoint, which requires the very `-shm` that is failing, so the plain pragma
CANNOT rescue it — that is what crash-looped the host at 12:31. The host store has no `locking_mode=EXCLUSIVE`
path, so a `host.db` already in WAL on the broken volume hits the same wall → **host crash-loop, whole grid
down** (ties to finding 032's residual 1). Also, `store.rs` sets no explicit `synchronous`/`busy_timeout` at
all (relies on defaults). Since it is still being written: port the engine's approach — try WAL, read back,
and if stuck use `locking_mode=EXCLUSIVE` to convert (heap wal-index, no `-shm`), then drop the lock; and set
`synchronous=FULL` (rollback path) + `busy_timeout` BEFORE the first write. F2 is the more urgent of the two
because a stuck `host.db` takes ALL tenants down, not one.

## Summary
Q1/Q2 recoverable (SQLite crash-atomic conversion + read-back guard), Q3 defeated (reap-by-uid before spawn) —
credit. Fix F1 (engine: FULL+busy before the conversion, one-line reorder) and F2 (host store: give it the
EXCLUSIVE escape hatch + explicit FULL, before it ships) so durability on the I/O-flaky volume does not rest
on SQLite's defaults.
