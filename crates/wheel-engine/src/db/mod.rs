//! sqlite layer.
//!
//! One writer connection behind a mutex — sqlite serialises writes anyway, and
//! a single writer makes the delivery loop's state transitions trivially
//! correct. User SQL never touches this connection: table nodes get a separate
//! read-only connection with an authorizer (`tables::query`).

use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::Connection;

pub mod board;
pub mod messages;
pub mod tables;
pub mod tokens;

/// Open (creating if needed) and migrate the project database.
pub fn open(path: &Path) -> Result<Connection> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating data dir {}", parent.display()))?;
    }
    let conn =
        Connection::open(path).with_context(|| format!("opening sqlite at {}", path.display()))?;
    configure(&conn)?;
    migrate(&conn)?;
    ensure_node_tables(&conn)?;
    Ok(conn)
}

/// Re-establish the sqlite table behind every table node, on every open.
///
/// The work is [`board::ensure_tables`]; this is only about WHERE it is
/// called. A concurrent session wired it into `serve`, which covers the engine
/// booting; opening the database is the narrower choke point and covers every
/// other way a project db is opened too, so the property holds unconditionally
/// rather than for one caller. Both call sites stand: it is idempotent, and
/// removing another session's integration mid-flight is the larger risk.
fn ensure_node_tables(conn: &Connection) -> Result<()> {
    board::ensure_tables(conn)
}

/// In-memory database, for tests.
pub fn open_memory() -> Result<Connection> {
    let conn = Connection::open_in_memory()?;
    configure(&conn)?;
    migrate(&conn)?;
    Ok(conn)
}

/// The sqlite journal mode this deployment can actually use.
///
/// WAL keeps its index in a `-shm` file. Railway's bind mount cannot RESIZE
/// one — "disk I/O error ... xShmMap" — and a resize happens as the index
/// grows, not when it is created, so no probe at open time can predict it. The
/// filesystem is a deployment fact, so it is configuration: `WHEEL_SQLITE_JOURNAL`,
/// WAL where nothing says otherwise.
pub fn journal_mode() -> String {
    std::env::var("WHEEL_SQLITE_JOURNAL")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| "WAL".into())
}

/// The journal mode the database is in right now, as sqlite reports it.
fn current_journal_mode(conn: &Connection) -> Result<String> {
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
fn set_journal_mode(conn: &Connection, wanted: &str) -> Result<String> {
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

fn configure(conn: &Connection) -> Result<()> {
    // A rollback journal is slower and still serves several connections, which
    // exclusive locking would not: the query path opens the file a second time.
    let mode = set_journal_mode(conn, &journal_mode())?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    // NORMAL means a lost transaction under WAL and a CORRUPT database under a
    // rollback journal, where there is no write-ahead log to replay. The
    // deployed volume runs on the rollback path, so it does not get the
    // weaker guarantee.
    let durability = if mode.eq_ignore_ascii_case("wal") {
        "NORMAL"
    } else {
        "FULL"
    };
    conn.pragma_update(None, "synchronous", durability)?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    Ok(())
}

fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(include_str!("schema.sql"))
        .context("applying schema")?;
    // Added after the first deploy, so it cannot live in schema.sql: those
    // statements are CREATE ... IF NOT EXISTS and never touch a table that
    // already exists.
    add_column(conn, "vault_values", "expires_at TEXT")?;
    Ok(())
}

/// Add a column that may already be there.
///
/// sqlite has no `ADD COLUMN IF NOT EXISTS`, and a database created before the
/// column existed is the normal case on a running deployment -- so a duplicate
/// column is the SUCCESS path here, not an error to report.
fn add_column(conn: &Connection, table: &str, decl: &str) -> Result<()> {
    match conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {decl}")) {
        Ok(()) => Ok(()),
        Err(e) if e.to_string().contains("duplicate column") => Ok(()),
        Err(e) => Err(e).with_context(|| format!("adding {table}.{decl}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table_node(name: &str, columns: &[&str]) -> wheel_core::Node {
        wheel_core::Node::new(
            uuid::Uuid::new_v4(),
            wheel_core::NodeName::new(name).unwrap(),
            wheel_core::Position::default(),
            wheel_core::NodeConfig::Table(wheel_core::TableConfig {
                columns: columns
                    .iter()
                    .map(|c| wheel_core::Column {
                        name: wheel_core::Ident::new(*c).unwrap(),
                        column_type: wheel_core::ColumnType::Text,
                    })
                    .collect(),
            }),
        )
    }

    /// PM's W1, from production: the wheel-dev board showed a `reports` table
    /// node and every `wheel read reports` answered "no such table: t_reports".
    /// The node survived; the table did not.
    #[test]
    fn a_table_node_whose_table_went_missing_gets_it_back_on_boot() {
        let dir = std::env::temp_dir().join(format!("wheel-ensure-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("wheel.db");

        let node = {
            let conn = open(&path).unwrap();
            let n = table_node("reports", &["title"]);
            crate::db::board::create(&conn, &n).unwrap();
            // Out-of-band, which is what a restore or a migration looks like
            // from in here.
            conn.execute_batch("DROP TABLE t_reports").unwrap();
            n
        };

        let conn = open(&path).unwrap();
        let cfg = match &node.config {
            wheel_core::NodeConfig::Table(c) => c,
            _ => unreachable!(),
        };
        let rows = tables::list_rows(&conn, &node.name, cfg, 10, 0)
            .expect("a read must not fail with \"no such table\" while the node exists");
        assert!(rows.is_empty(), "restored empty, not populated from nowhere");

        // And it is the node's own schema, not a default one.
        tables::put_row(
            &conn,
            &node.name,
            cfg,
            "r1",
            &serde_json::json!({ "title": "hello" }),
        )
        .expect("the restored table must accept the node's configured columns");

        drop(conn);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A column added to a table node's config after the table was built.
    /// Boot reconciles it; the rows already there are kept.
    #[test]
    fn a_column_added_to_the_config_appears_after_a_restart_without_losing_rows() {
        let dir = std::env::temp_dir().join(format!("wheel-ensure-col-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("wheel.db");

        let mut node = table_node("reports", &["title"]);
        {
            let conn = open(&path).unwrap();
            crate::db::board::create(&conn, &node).unwrap();
            let cfg = match &node.config {
                wheel_core::NodeConfig::Table(c) => c,
                _ => unreachable!(),
            };
            tables::put_row(&conn, &node.name, cfg, "r1", &serde_json::json!({"title":"kept"}))
                .unwrap();
        }

        node.config = wheel_core::NodeConfig::Table(wheel_core::TableConfig {
            columns: vec![
                wheel_core::Column {
                    name: wheel_core::Ident::new("title").unwrap(),
                    column_type: wheel_core::ColumnType::Text,
                },
                wheel_core::Column {
                    name: wheel_core::Ident::new("body").unwrap(),
                    column_type: wheel_core::ColumnType::Text,
                },
            ],
        });
        {
            let conn = open(&path).unwrap();
            crate::db::board::update(&conn, &node).unwrap();
        }

        let conn = open(&path).unwrap();
        let cfg = match &node.config {
            wheel_core::NodeConfig::Table(c) => c,
            _ => unreachable!(),
        };
        tables::put_row(&conn, &node.name, cfg, "r2", &serde_json::json!({"title":"t","body":"b"}))
            .expect("the new column must be there after a restart");
        assert_eq!(
            tables::list_rows(&conn, &node.name, cfg, 10, 0).unwrap().len(),
            2,
            "reconciling columns must not discard rows"
        );

        drop(conn);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn schema_applies_and_is_reentrant() {
        let conn = open_memory().unwrap();
        // migrate runs on every boot, including after a crash mid-write.
        migrate(&conn).unwrap();

        let tables: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        for expected in [
            "agent_state",
            "chest_index",
            "logs",
            "messages",
            "node_tokens",
            "nodes",
            "vault_values",
            "wires",
        ] {
            assert!(tables.contains(&expected.to_string()), "missing {expected}");
        }
    }

    #[test]
    fn foreign_keys_are_on_so_deleting_a_node_cascades() {
        let conn = open_memory().unwrap();
        let on: i64 = conn
            .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
            .unwrap();
        assert_eq!(on, 1, "foreign_keys must be ON: node deletion relies on it");

        let now = "2026-09-05T00:00:00Z";
        conn.execute(
            "INSERT INTO nodes (id,name,type,config,x,y,created_at,updated_at)
             VALUES ('n1','a','agent','{}',0,0,?1,?1)",
            [now],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO agent_state (node_id,status) VALUES ('n1','stopped')",
            [],
        )
        .unwrap();
        conn.execute("DELETE FROM nodes WHERE id='n1'", []).unwrap();

        let left: i64 = conn
            .query_row("SELECT count(*) FROM agent_state", [], |r| r.get(0))
            .unwrap();
        assert_eq!(left, 0, "agent_state must cascade with its node");
    }

    #[test]
    fn a_wire_cannot_reference_a_node_that_does_not_exist() {
        let conn = open_memory().unwrap();
        let r = conn.execute(
            "INSERT INTO wires (from_id,to_id,type,created_at)
             VALUES ('ghost','other','send','2026-09-05T00:00:00Z')",
            [],
        );
        assert!(r.is_err(), "dangling wires must be refused by the schema");
    }

    #[test]
    fn node_names_are_unique_at_the_storage_layer_too() {
        let conn = open_memory().unwrap();
        let now = "2026-09-05T00:00:00Z";
        let insert = |id: &str, name: &str| {
            conn.execute(
                "INSERT INTO nodes (id,name,type,config,x,y,created_at,updated_at)
                 VALUES (?1,?2,'ctx','{}',0,0,?3,?3)",
                rusqlite::params![id, name, now],
            )
        };
        insert("n1", "notes").unwrap();
        // Uniqueness is enforced in the engine, but a bug there must not be
        // able to produce two nodes answering to one address.
        assert!(insert("n2", "notes").is_err());
    }
}

#[cfg(test)]
mod storage_mode_tests {
    use super::*;

    /// The engine opens a project database more than once -- `tables::query`
    /// opens the file by path -- so any scheme that makes the first connection
    /// exclusive locks the agent-facing query path out. I shipped exactly that
    /// mistake while chasing the shared-memory crash; this is the guard.
    #[test]
    fn a_second_connection_can_read_while_the_first_is_open() {
        let dir = std::env::temp_dir().join(format!("wheel-db-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("wheel.db");
        let first = open(&path).unwrap();
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
        // SAFETY: single-threaded test process; restored before returning.
        unsafe { std::env::set_var("WHEEL_SQLITE_JOURNAL", "TRUNCATE") };
        let conn = open(&path).unwrap();
        unsafe { std::env::remove_var("WHEEL_SQLITE_JOURNAL") };

        assert_eq!(current_journal_mode(&conn).unwrap(), "truncate");
        let sync: i64 = conn
            .query_row("PRAGMA synchronous", [], |r| r.get(0))
            .unwrap();
        assert_eq!(sync, 2, "expected synchronous=FULL (2) on a rollback journal");

        std::fs::remove_dir_all(&dir).ok();
    }
}

#[cfg(test)]
mod journal_mode_tests {
    use super::*;

    /// A deployment whose volume cannot host a WAL index must be able to say so
    /// without a code change. The engine crash-looped for 20 minutes because a
    /// probe cannot predict a resize that happens later.
    #[test]
    fn the_journal_mode_is_configuration_with_wal_as_the_default() {
        let restore = std::env::var("WHEEL_SQLITE_JOURNAL").ok();
        unsafe { std::env::remove_var("WHEEL_SQLITE_JOURNAL") };
        assert_eq!(journal_mode(), "WAL");
        unsafe { std::env::set_var("WHEEL_SQLITE_JOURNAL", "TRUNCATE") };
        assert_eq!(journal_mode(), "TRUNCATE");
        unsafe { std::env::set_var("WHEEL_SQLITE_JOURNAL", "  ") };
        assert_eq!(journal_mode(), "WAL", "a blank setting is not a mode");
        match restore {
            Some(v) => unsafe { std::env::set_var("WHEEL_SQLITE_JOURNAL", v) },
            None => unsafe { std::env::remove_var("WHEEL_SQLITE_JOURNAL") },
        }
    }
}
