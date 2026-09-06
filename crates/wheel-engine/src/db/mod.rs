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
    Ok(conn)
}

/// In-memory database, for tests.
pub fn open_memory() -> Result<Connection> {
    let conn = Connection::open_in_memory()?;
    configure(&conn)?;
    migrate(&conn)?;
    Ok(conn)
}

fn configure(conn: &Connection) -> Result<()> {
    // WAL so a reader never blocks the delivery loop's writes.
    conn.pragma_update(None, "journal_mode", "WAL")?;
    // Set per connection, and node deletion relies on ON DELETE CASCADE, so
    // this is load-bearing rather than tuning.
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
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
