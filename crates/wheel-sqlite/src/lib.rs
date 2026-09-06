//! The one place that decides how a Wheel sqlite database is journalled.
//!
//! Shared by `wheel-engine` (a project's board) and `wheel-host` (its own
//! record of which projects exist). It is one crate rather than a function in
//! each because the two had already drifted: the engine grew a drain for a
//! database stuck in WAL while the host kept the version that crash-looped on
//! it, and the host's store is what opens FIRST on the deployed machine. Two
//! copies of this would diverge again, and the second one would be the one
//! nobody tests.

use anyhow::Result;
use rusqlite::Connection;

/// How long to wait for a lock rather than failing instantly, in ms.
///
/// sqlite's default is zero. Both databases here are opened by more than one
/// connection, and the host's is opened while a previous engine's processes
/// may still be releasing locks.
const BUSY_TIMEOUT_MS: i64 = 5_000;

/// The sqlite journal mode this deployment can actually use.
///
/// WAL keeps its index in a `-shm` file. Railway's bind mount cannot RESIZE
/// one — "disk I/O error ... xShmMap" — and a resize happens as the index
/// grows, not when it is created, so no probe at open time can predict it. The
/// filesystem is a deployment fact, so it is configuration: `WHEEL_SQLITE_JOURNAL`,
/// WAL where nothing says otherwise.
pub fn target_mode() -> String {
    std::env::var("WHEEL_SQLITE_JOURNAL")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| "WAL".into())
}

/// The journal mode the database is in right now, as sqlite reports it.
pub fn current_journal_mode(conn: &Connection) -> Result<String> {
    Ok(conn.query_row("PRAGMA journal_mode", [], |r| r.get::<_, String>(0))?)
}

/// Put the database into `wanted`, and return the mode actually in force.
///
/// `pragma_update(journal_mode, ...)` is NOT a reliable signal: sqlite answers
/// with the RESULTING mode rather than failing, so it returns `Ok` for a mode
/// it did not enter -- measured, `journal_mode = nonsense-mode` returns Ok and
/// leaves the mode untouched. Reading the mode back is the only proof, which
/// is why every decision here is made on `current_journal_mode`.
///
/// The stuck case this exists for: a database ALREADY in WAL on a volume whose
/// `-shm` cannot be mapped or resized. `journal_mode` persists in the file
/// header, so every boot rediscovers WAL; and LEAVING WAL requires
/// checkpointing it, which requires the very wal-index that is failing. The
/// plain pragma therefore cannot rescue the one database that most needs it,
/// and returning its error crash-loops the host -- which is what took
/// production down at 12:31.
///
/// `locking_mode = EXCLUSIVE` is sqlite's documented escape hatch: with it,
/// WAL keeps its index in HEAP MEMORY and needs no `-shm` at all. Exclusive
/// locking cannot be left on -- `tables::query` opens the file a second time
/// and would be locked out (7 tests caught exactly that) -- so it is held only
/// across the conversion and dropped again.
pub fn set_journal_mode(conn: &Connection, wanted: &str) -> Result<String> {
    let _ = conn.pragma_update(None, "journal_mode", wanted);
    let mode = current_journal_mode(conn)?;
    if mode.eq_ignore_ascii_case(wanted) {
        return Ok(mode);
    }
    // An in-memory database has no file to journal and always reports
    // `memory`; it cannot be WAL and is not failing to be. Every other
    // mismatch is a database that would not go where we asked it to.
    if mode.eq_ignore_ascii_case("memory") {
        return Ok(mode);
    }

    drain_under_exclusive_lock(conn, wanted)?;

    // Read back, for the same reason as above: the pragma's own result is not
    // evidence. My first draft of this function returned that result here and
    // reported success for a mode sqlite had refused -- the exact mistake this
    // function exists to stop, made inside it.
    let settled = current_journal_mode(conn)?;
    anyhow::ensure!(
        settled.eq_ignore_ascii_case(wanted),
        "could not put the database into {wanted}; it is in {settled}"
    );
    Ok(settled)
}

/// Convert the journal mode while holding sqlite's exclusive lock, then give
/// the lock back.
///
/// Its own function because **its call site above cannot be reached in a
/// test**: the plain pragma only fails on a filesystem whose `-shm` cannot be
/// mapped or resized, and nothing local reproduces that. A test that drove
/// `set_journal_mode` on a healthy disk would take the early return, pass, and
/// prove nothing about this code -- I checked, by deleting this body and
/// watching the suite stay green. So the drain is tested directly instead, and
/// the honest statement of coverage is: the conversion is gated, the decision
/// to reach for it is not.
fn drain_under_exclusive_lock(conn: &Connection, wanted: &str) -> Result<()> {
    let _ = conn.pragma_update(None, "locking_mode", "EXCLUSIVE");
    let _ = conn.pragma_update(None, "journal_mode", wanted);
    // Back to normal locking unconditionally: leaving a connection exclusive
    // because the conversion failed would trade a crash loop for an engine no
    // second connection can read.
    let _ = conn.pragma_update(None, "locking_mode", "NORMAL");
    // sqlite holds the exclusive lock until the database is next unlocked, so
    // dropping the pragma is not enough on its own -- a transaction has to
    // complete for the lock to actually be released.
    let _ = conn.execute_batch("BEGIN IMMEDIATE; COMMIT;");
    Ok(())
}


/// Put a database into the mode this deployment can actually use, and give it
/// the durability setting that mode requires. Returns the live mode.
///
/// Both callers do this and nothing else about journalling, so it is one call.
pub fn configure_journal(conn: &Connection) -> Result<String> {
    configure_journal_to(conn, &target_mode())
}

/// [`configure_journal`] with the target named explicitly.
///
/// Exists so tests do not have to reach for `WHEEL_SQLITE_JOURNAL`: the
/// environment is process-global and cargo runs tests in parallel, so one test
/// setting it changes what a sibling sees. That is not hypothetical -- it is
/// how the first version of the sequence test below broke an unrelated one.
pub fn configure_journal_to(conn: &Connection, wanted: &str) -> Result<String> {
    // ORDER IS THE POINT (ADVERSARY 033 F1). The conversion below performs a
    // checkpoint -- the single riskiest write this process makes on a volume
    // that is already failing -- so it must not run at sqlite's defaults.
    //
    // busy_timeout defaults to ZERO: no tolerance at all for a second
    // connection (`tables::query` opens the file again) or a lock a reaped
    // process has not finished releasing. And synchronous defaults below FULL,
    // which is the wrong guarantee for a checkpoint into a rollback journal
    // that has no write-ahead log to replay. Both are therefore set BEFORE the
    // conversion, not after it, which is where they used to sit.
    // Set as SQL rather than through `Connection::busy_timeout`, which is a C
    // call and leaves no statement behind: the ORDER of these two relative to
    // the conversion is the finding, and QA measured that no black-box test
    // can observe it (both pragmas are per-connection and leave no trace, and
    // the lock a journal-mode change needs is one busy_timeout does not help
    // with, so the two orders are externally identical). As SQL they are at
    // least visible to sqlite's own trace hook, which is what gates them below.
    conn.pragma_update(None, "busy_timeout", BUSY_TIMEOUT_MS)?;
    conn.pragma_update(None, "synchronous", "FULL")?;

    let mode = set_journal_mode(conn, wanted)?;

    // Only now, and only if WAL actually holds, is the weaker setting safe:
    // under WAL, NORMAL costs a recent transaction on power loss, while under
    // a rollback journal it risks a corrupt database.
    if mode.eq_ignore_ascii_case("wal") {
        conn.pragma_update(None, "synchronous", "NORMAL")?;
    }
    Ok(mode)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    /// The engine opens a project database more than once -- `tables::query`
    /// opens the file by path -- so any scheme that makes the first connection
    /// exclusive locks the agent-facing query path out. I shipped exactly that
    /// mistake while chasing the shared-memory crash; this is the guard.
    #[test]
    fn a_second_connection_can_read_while_the_first_is_open() {
        let dir = std::env::temp_dir().join(format!("wheel-db-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("wheel.db");
        let first = Connection::open(&path).unwrap();
        configure_journal(&first).unwrap();
        first.execute("CREATE TABLE t (a TEXT)", []).unwrap();
        first.execute("INSERT INTO t VALUES ('x')", []).unwrap();

        let second = Connection::open(&path).unwrap();
        let n: i64 = second
            .query_row("SELECT count(*) FROM t", [], |r| r.get(0))
            .expect("a second connection must be able to read the project database");
        assert_eq!(n, 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The deployed shape: a database ALREADY in WAL that has to be converted
    /// to a rollback journal. On the volume that took production down, the
    /// plain pragma cannot do this -- leaving WAL needs a checkpoint, which
    /// needs the wal-index that is failing -- so the conversion runs under a
    /// transient exclusive lock, where sqlite keeps that index in heap memory.
    ///
    /// The assertions that matter are the last two. Converting is easy; giving
    /// the exclusive lock BACK is the part that rots, and an engine that holds
    /// it has traded a crash loop for a database no second connection can
    /// read -- which is how `tables::query`, the agent-facing path, dies.
    #[test]
    fn a_wal_database_is_converted_and_the_exclusive_lock_is_given_back() {
        let dir = std::env::temp_dir().join(format!("wheel-db-drain-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("wheel.db");
        {
            let seed = Connection::open(&path).unwrap();
            seed.pragma_update(None, "journal_mode", "WAL").unwrap();
            seed.execute_batch("CREATE TABLE t (a TEXT); INSERT INTO t VALUES ('y');")
                .unwrap();
            assert_eq!(current_journal_mode(&seed).unwrap(), "wal");
        }

        let conn = Connection::open(&path).unwrap();
        // The drain directly: `set_journal_mode` would convert this with the
        // plain pragma on a healthy disk and never reach the code under test.
        drain_under_exclusive_lock(&conn, "TRUNCATE").unwrap();
        let mode = current_journal_mode(&conn).unwrap();

        assert_eq!(mode, "truncate", "the database was left in {mode}");
        let n: i64 = conn
            .query_row("SELECT count(*) FROM t", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1, "the rows in the WAL were not carried over");

        // The lock is back: another connection can not only read but WRITE.
        let second = Connection::open(&path).unwrap();
        second
            .execute("INSERT INTO t VALUES ('z')", [])
            .expect("a second connection is locked out -- the exclusive lock was kept");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// `pragma_update` answers with the resulting mode instead of failing, so
    /// it returns `Ok` for a mode it did not enter. Anything that trusts that
    /// result reports a database is in a mode it is not in.
    #[test]
    fn a_mode_sqlite_refuses_is_reported_as_the_mode_it_actually_kept() {
        let dir = std::env::temp_dir().join(format!("wheel-db-bogus-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("wheel.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch("CREATE TABLE t (a TEXT)").unwrap();

        assert!(
            conn.pragma_update(None, "journal_mode", "nonsense-mode")
                .is_ok(),
            "premise of this test: sqlite does not fail on a mode it refuses"
        );

        let reported = set_journal_mode(&conn, "nonsense-mode").unwrap_err();
        assert!(
            reported.to_string().contains("could not put the database"),
            "a refused mode must be reported as refused, not as success: {reported}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A rollback journal has no write-ahead log to replay, so `NORMAL` there
    /// risks a CORRUPT database rather than a lost transaction. The deployed
    /// volume runs on this path.
    #[test]
    fn a_rollback_journal_database_gets_the_stronger_durability_setting() {
        let dir = std::env::temp_dir().join(format!("wheel-db-sync-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("wheel.db");
        let conn = Connection::open(&path).unwrap();
        configure_journal_to(&conn, "TRUNCATE").unwrap();

        assert_eq!(current_journal_mode(&conn).unwrap(), "truncate");
        let sync: i64 = conn
            .query_row("PRAGMA synchronous", [], |r| r.get(0))
            .unwrap();
        assert_eq!(sync, 2, "expected synchronous=FULL (2) on a rollback journal");

        std::fs::remove_dir_all(&dir).ok();
    }
}

#[cfg(test)]
mod target_mode_tests {
    use super::*;

    /// A deployment whose volume cannot host a WAL index must be able to say so
    /// without a code change. The engine crash-looped for 20 minutes because a
    /// probe cannot predict a resize that happens later.
    #[test]
    fn the_journal_mode_is_configuration_with_wal_as_the_default() {
        let restore = std::env::var("WHEEL_SQLITE_JOURNAL").ok();
        unsafe { std::env::remove_var("WHEEL_SQLITE_JOURNAL") };
        assert_eq!(target_mode(), "WAL");
        unsafe { std::env::set_var("WHEEL_SQLITE_JOURNAL", "TRUNCATE") };
        assert_eq!(target_mode(), "TRUNCATE");
        unsafe { std::env::set_var("WHEEL_SQLITE_JOURNAL", "  ") };
        assert_eq!(target_mode(), "WAL", "a blank setting is not a mode");
        match restore {
            Some(v) => unsafe { std::env::set_var("WHEEL_SQLITE_JOURNAL", v) },
            None => unsafe { std::env::remove_var("WHEEL_SQLITE_JOURNAL") },
        }
    }
}

#[cfg(test)]
mod ordering_tests {
    use super::*;
    use rusqlite::Connection;

    /// ADVERSARY 033 F1. The conversion checkpoints, and a checkpoint is the
    /// riskiest write we make on a volume that is already failing, so it must
    /// not run at sqlite's defaults: busy_timeout starts at ZERO (no tolerance
    /// for a second connection or a lock a reaped process still holds) and
    /// synchronous starts below FULL.
    ///
    /// Both settings survive to the end on a rollback journal, which is what
    /// this asserts; the ordering itself is stated where it is enforced. On a
    /// database that ends in WAL, `synchronous` is deliberately relaxed after
    /// the conversion, and that is the other half of the pair.
    #[test]
    fn a_converted_database_keeps_full_durability_and_a_real_busy_timeout() {
        let dir = std::env::temp_dir().join(format!("wheel-sql-ord-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("x.db");
        {
            let seed = Connection::open(&path).unwrap();
            seed.pragma_update(None, "journal_mode", "WAL").unwrap();
            seed.execute_batch("CREATE TABLE t (a)").unwrap();
        }
        let conn = Connection::open(&path).unwrap();
        let mode = configure_journal_to(&conn, "TRUNCATE").unwrap();

        assert_eq!(mode, "truncate");
        let sync: i64 = conn
            .query_row("PRAGMA synchronous", [], |r| r.get(0))
            .unwrap();
        assert_eq!(sync, 2, "a rollback journal must keep synchronous=FULL (2)");
        let busy: i64 = conn
            .query_row("PRAGMA busy_timeout", [], |r| r.get(0))
            .unwrap();
        assert!(busy > 0, "busy_timeout was left at sqlite's zero default");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// The other half: WAL keeps the cheaper setting, so the pair cannot be
    /// "fixed" by simply hard-coding FULL everywhere.
    #[test]
    fn a_wal_database_is_relaxed_to_normal_after_the_conversion() {
        let dir = std::env::temp_dir().join(format!("wheel-sql-wal-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("x.db");
        let conn = Connection::open(&path).unwrap();
        let mode = configure_journal_to(&conn, "WAL").unwrap();
        assert_eq!(mode, "wal");
        let sync: i64 = conn
            .query_row("PRAGMA synchronous", [], |r| r.get(0))
            .unwrap();
        assert_eq!(sync, 1, "WAL should end at synchronous=NORMAL (1)");
        std::fs::remove_dir_all(&dir).ok();
    }
}

#[cfg(test)]
mod sequence_tests {
    use super::*;
    use rusqlite::Connection;
    use std::cell::RefCell;

    thread_local! {
        static TRACE: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    }

    fn record(sql: &str) {
        TRACE.with(|t| t.borrow_mut().push(sql.to_lowercase()));
    }

    /// ADVERSARY 033 F1, and QA's ENG-pragma-order, which they explicitly could
    /// not gate from outside: they measured that a conversion under contention
    /// fails identically at busy_timeout=0 and 3000, so no external experiment
    /// separates the two orders. This one can, because it watches the
    /// statements go past.
    ///
    /// Why it matters: the conversion checkpoints, and that is the riskiest
    /// write this process makes on a volume that is already failing. Running it
    /// at sqlite's defaults means zero tolerance for a second connection
    /// (`tables::query` opens the file again) and a weaker durability
    /// guarantee than a rollback journal needs.
    #[test]
    fn durability_and_lock_tolerance_are_set_before_the_conversion() {
        let dir = std::env::temp_dir().join(format!("wheel-sql-seq-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("x.db");
        {
            let seed = Connection::open(&path).unwrap();
            seed.pragma_update(None, "journal_mode", "WAL").unwrap();
            seed.execute_batch("CREATE TABLE t (a)").unwrap();
        }

        TRACE.with(|t| t.borrow_mut().clear());
        let mut conn = Connection::open(&path).unwrap();
        conn.trace(Some(record));
        configure_journal_to(&conn, "TRUNCATE").unwrap();
        conn.trace(None);

        let sql = TRACE.with(|t| t.borrow().clone());
        let first = |needle: &str| {
            sql.iter()
                .position(|s| s.contains(needle))
                .unwrap_or_else(|| panic!("no statement mentioning {needle:?} ran:\n{sql:#?}"))
        };
        let conversion = first("journal_mode");
        assert!(
            first("busy_timeout") < conversion,
            "busy_timeout was set AFTER the conversion, so the conversion ran with \
             sqlite's zero default:\n{sql:#?}"
        );
        assert!(
            first("synchronous") < conversion,
            "synchronous was set AFTER the conversion, so the checkpoint ran at \
             sqlite's weaker default:\n{sql:#?}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }
}
