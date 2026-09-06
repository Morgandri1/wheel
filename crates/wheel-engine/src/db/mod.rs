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
    // `false` = never take the file exclusively. The board's own query path
    // (`tables::query`) opens it a second time, so an exclusive engine is an
    // engine whose agents cannot read their own tables -- a hard error naming
    // the mode the database is stuck in is the better failure.
    //
    // `open_configured` already negotiates the journal mode -- including the
    // slow escalation path on a volume that cannot host WAL (BEGIN IMMEDIATE
    // write-proofs, an exclusive drain, a retry, all real I/O). Running
    // `configure`'s `configure_journal` a SECOND time here, on the connection
    // it just returned, repeated every one of those slow attempts for no
    // reason: this connection is already in a working mode. On a hostile
    // volume that doubled the boot's worst-case latency, which is what pushed
    // it past the CI fixture's patience (`ENG-journal-override-cannot-
    // disable-recovery`, which timed the container out rather than seeing it
    // stay unhealthy -- the engine's own log shows it reaching "listening"
    // every time, just too late). `foreign_keys` still needs setting; it does
    // not need the journal negotiated a second time to get it.
    let conn = wheel_sqlite::open_configured(&path.display().to_string(), false)?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
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

fn configure(conn: &Connection) -> Result<()> {
    // Journalling is `wheel-sqlite`'s: the host's store makes exactly the same
    // decision and the two copies had already drifted apart.
    wheel_sqlite::configure_journal(conn)?;
    // Set per connection, and node deletion relies on ON DELETE CASCADE, so
    // this is load-bearing rather than tuning.
    // busy_timeout and synchronous are set by `configure_journal`, BEFORE the
    // conversion it may have to perform (ADVERSARY 033 F1) -- they were here,
    // after it, which left the riskiest write on this volume running at
    // sqlite's zero-tolerance defaults.
    conn.pragma_update(None, "foreign_keys", "ON")?;
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
        assert!(
            rows.is_empty(),
            "restored empty, not populated from nowhere"
        );

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
            tables::put_row(
                &conn,
                &node.name,
                cfg,
                "r1",
                &serde_json::json!({"title":"kept"}),
            )
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
        tables::put_row(
            &conn,
            &node.name,
            cfg,
            "r2",
            &serde_json::json!({"title":"t","body":"b"}),
        )
        .expect("the new column must be there after a restart");
        assert_eq!(
            tables::list_rows(&conn, &node.name, cfg, 10, 0)
                .unwrap()
                .len(),
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
