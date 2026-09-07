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
    snap_positions_to_cells(conn)?;
    Ok(())
}

/// Round and clamp stored positions to whole cells (ARCHITECTURE.md "Position
/// is an integer cell").
///
/// `Position` became `i16` in the type, and the read path rounds whatever it
/// finds, so the board already answers with integers. This is about the bytes
/// at rest: without it, rows written before the ruling keep `10.4` for ever and
/// anything that reads the column without going through `Position` -- an
/// export, a backup, a hand-written query -- still sees a float. Rounding once,
/// on boot, is what makes the stored value and the served value the same value.
///
/// The `x`/`y` columns stay `REAL` deliberately. Seven tables reference
/// `nodes(id)` with `ON DELETE CASCADE`, and sqlite cannot change a column type
/// in place: the rebuild is rename-create-copy-drop, during which those foreign
/// keys follow the rename onto the old table and the drop cascades away every
/// wire, message, and vault row in the project. Nothing writes a fraction --
/// the only writer binds an `i16` -- so the column's width buys nothing worth
/// that risk.
fn snap_positions_to_cells(conn: &Connection) -> Result<()> {
    // Name the ones that CLAMP, before changing them (BUG-029).
    //
    // Rounding cannot move a node anywhere an operator can see: half a cell per
    // axis is 1.27 px at the board's maximum zoom. Clamping is the only
    // unbounded case — a node stored at (99999, -99999) lands on the bound,
    // which is a move of 67,232 cells, about 121,000 px. It teleports across
    // the screen, and the only line in the log used to be a count.
    for c in positions_that_will_clamp(conn)? {
        tracing::warn!(
            node = %c.name,
            from_x = c.from_x, from_y = c.from_y, to_x = c.to_x, to_y = c.to_y,
            moved_cells = c.moved_cells(),
            "position was outside the board's bounds and has been clamped; this node MOVED, \
             and it is the only migration case an operator can see"
        );
    }

    let snapped = conn
        .execute(
            "UPDATE nodes
                SET x = MAX(-32768, MIN(32767, CAST(round(x) AS INTEGER))),
                    y = MAX(-32768, MIN(32767, CAST(round(y) AS INTEGER)))
              WHERE x <> MAX(-32768, MIN(32767, CAST(round(x) AS INTEGER)))
                 OR y <> MAX(-32768, MIN(32767, CAST(round(y) AS INTEGER)))",
            [],
        )
        .context("snapping stored positions to whole cells")?;
    if snapped > 0 {
        tracing::info!(nodes = snapped, "snapped stored positions to whole cells");
    }
    Ok(())
}

/// A node whose stored position is outside the board's bounds, and where it
/// will land.
///
/// Separated from the logging so the thing worth asserting — WHICH nodes, and
/// from where to where — is a value a test can hold, rather than a line a test
/// has to scrape out of a subscriber.
#[derive(Debug, PartialEq)]
pub(crate) struct ClampReport {
    pub name: String,
    pub from_x: f64,
    pub from_y: f64,
    pub to_x: i64,
    pub to_y: i64,
}

impl ClampReport {
    /// The larger of the two axis moves, in cells — what an operator sees.
    pub fn moved_cells(&self) -> f64 {
        (self.from_x - self.to_x as f64)
            .abs()
            .max((self.from_y - self.to_y as f64).abs())
    }
}

/// Read BEFORE the update: afterwards the old position is gone, and a report
/// could only name the destination — which is the half an operator can already
/// see on the board.
pub(crate) fn positions_that_will_clamp(conn: &Connection) -> Result<Vec<ClampReport>> {
    let mut stmt = conn.prepare(
        "SELECT name, x, y FROM nodes
          WHERE round(x) < -32768 OR round(x) > 32767
             OR round(y) < -32768 OR round(y) > 32767
          ORDER BY name",
    )?;
    let rows = stmt.query_map([], |r| {
        let x: f64 = r.get("x")?;
        let y: f64 = r.get("y")?;
        Ok(ClampReport {
            name: r.get("name")?,
            from_x: x,
            from_y: y,
            to_x: x.round().clamp(-32768.0, 32767.0) as i64,
            to_y: y.round().clamp(-32768.0, 32767.0) as i64,
        })
    })?;
    Ok(rows.flatten().collect())
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

    /// BUG-029: clamping is the ONLY migration case an operator can see, and it
    /// happened without a word.
    ///
    /// Rounding moves a node at most half a cell per axis — 1.27 px at the
    /// board's max zoom of 1.8, which nobody notices and which therefore needs
    /// no report. A stored (99999, -99999) lands on the bound: 67,232 cells,
    /// about 121,000 px. The node teleports across the screen and the boot log
    /// said only "snapped stored positions to whole cells".
    ///
    /// Asserted on the reported VALUE rather than on the log line, so what is
    /// pinned is which nodes are named and where they came from — the part a
    /// operator needs — instead of the formatting.
    #[test]
    fn a_migration_names_every_node_it_moves_visibly_and_no_others() {
        let conn = open_memory().unwrap();
        let place = |name: &str, x: f64, y: f64| {
            conn.execute(
                "INSERT INTO nodes (id, name, type, config, x, y, created_at, updated_at)
                 VALUES (?1, ?2, 'ctx', '{\"markdown\":\"\"}', ?3, ?4, '', '')",
                rusqlite::params![uuid::Uuid::new_v4().to_string(), name, x, y],
            )
            .unwrap();
        };
        place("rounds-only", 10.6, -10.6);
        place("on-the-bound", 32767.0, -32768.0);
        place("way-off", 99999.0, -99999.4);

        let reported = positions_that_will_clamp(&conn).unwrap();

        assert_eq!(
            reported.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
            vec!["way-off"],
            "only a node that actually leaves the bounds may be reported: rounding is \
             invisible and a node already ON the bound does not move"
        );
        let c = &reported[0];
        assert_eq!((c.to_x, c.to_y), (32767, -32768));
        assert_eq!(
            c.from_x, 99999.0,
            "the report must carry where it came FROM"
        );
        assert!(
            c.moved_cells() > 67_000.0,
            "the distance is the point — {} cells is what the operator sees",
            c.moved_cells()
        );
    }

    /// CI, POS-migration/engine-restarts: the engine did not come back up after
    /// float rows were seeded, and said only
    /// "Conversion error from type Text at index: 0, invalid character: found `m` at 0".
    ///
    /// The seeded rows carried ids like `mig-0000` rather than uuids. Boot calls
    /// `board::ensure_tables`, which used `board::list`, which parses every
    /// row's id and fails WHOLE on the first one it cannot read — so one
    /// malformed row stopped the engine from starting, with an error naming
    /// neither the row nor the column.
    ///
    /// A fixture is the friendly version of this. A partial restore, or a row
    /// written by a tool that did not know better, is the unfriendly one, and
    /// an engine that refuses to boot is a much worse outcome than the row.
    #[test]
    fn one_unreadable_node_row_does_not_stop_the_engine_from_starting() {
        let dir = std::env::temp_dir().join(format!("wheel-badrow-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("wheel.db");
        let _ = std::fs::remove_file(&path);

        {
            let conn = open(&path).unwrap();
            let good = table_node("reports", &["title"]);
            crate::db::board::create(&conn, &good).unwrap();
            conn.execute(
                "INSERT INTO nodes (id, name, type, config, x, y, created_at, updated_at)
                 VALUES ('mig-0000', 'mig-node-0000', 'ctx', '{\"markdown\":\"seeded\"}', 1.0, 2.0, '', '')",
                [],
            )
            .unwrap();
            conn.execute_batch("DROP TABLE t_reports").unwrap();
        }

        // The boot that CI could not complete.
        let conn = open(&path).expect("one unreadable row must not stop the engine booting");

        // And the repair still ran for the rows it COULD read: skipping the bad
        // row must not mean skipping the rest.
        let restored: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='t_reports'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            restored, 1,
            "the good table node's storage must still be re-ensured past the bad row"
        );

        let _ = std::fs::remove_file(&path);
    }

    /// The operator's board carried 20 nodes with fractional positions when
    /// the ruling landed. Reading rounds them, so the API looked correct while
    /// the bytes on the volume stayed fractional for ever -- which is the half
    /// an export, a backup or a hand-written query would still have seen.
    ///
    /// Written against the STORED value on purpose: asserting through
    /// `board::get` would pass with no migration at all, because the read path
    /// rounds.
    #[test]
    fn positions_stored_before_the_ruling_are_whole_cells_after_a_boot() {
        let dir = std::env::temp_dir().join(format!("wheel-snap-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("wheel.db");
        let _ = std::fs::remove_file(&path);

        let id = uuid::Uuid::new_v4().to_string();
        {
            let conn = open(&path).unwrap();
            // Bound as f64 to get past `Position` entirely: this is the shape a
            // row written before the type change has.
            conn.execute(
                "INSERT INTO nodes (id, name, type, config, x, y, created_at, updated_at)
                 VALUES (?1, 'legacy', 'ctx', '{\"markdown\":\"\"}', ?2, ?3, '', '')",
                rusqlite::params![id, -10.6_f64, 99999.4_f64],
            )
            .unwrap();
        }

        let conn = open(&path).unwrap();
        let (x, y): (f64, f64) = conn
            .query_row("SELECT x, y FROM nodes WHERE id = ?1", [&id], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(x, -11.0, "-10.6 rounds away from zero, in the stored value");
        assert_eq!(y, 32767.0, "past the bound it clamps rather than wrapping");

        let _ = std::fs::remove_file(&path);
    }

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
