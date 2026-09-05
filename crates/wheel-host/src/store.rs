//! The host's own durable state: which projects exist, their secrets, and whether they are
//! *supposed* to be running.
//!
//! This is what makes reconciliation-on-boot possible. The host is a single instance by design, so
//! if it restarts, the only record that project X should be running is here.

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension};
use uuid::Uuid;

pub struct Store {
    conn: tokio::sync::Mutex<Connection>,
}

/// Note: `desired_running` is not a field here. It is a query predicate — `all_desired_running`
/// filters on it in SQL — and carrying a stale copy around in memory would invite acting on it.
#[derive(Debug, Clone)]
pub struct ProjectRecord {
    pub id: Uuid,
    /// Base uid of this project's range. `None` until allocated (the docker backend never needs one).
    pub uid_base: Option<u32>,
    pub engine_secret: String,
    pub vault_key: String,
}

impl Store {
    pub fn open(path: &str) -> Result<Self> {
        let conn = Connection::open(path).with_context(|| format!("opening {path}"))?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE IF NOT EXISTS projects (
               id               TEXT PRIMARY KEY,
               engine_secret    TEXT NOT NULL,
               vault_key        TEXT NOT NULL,
               desired_running  INTEGER NOT NULL DEFAULT 0,
               -- Base uid of this project's range, for the process backend. UNIQUE because two
               -- projects sharing a uid would mean two tenants sharing a filesystem identity.
               uid_base         INTEGER UNIQUE
             );",
        )?;
        // Migrate databases created before uid allocation existed. sqlite has no
        // ADD COLUMN IF NOT EXISTS, so a duplicate-column error here is the success case.
        let _ = conn.execute("ALTER TABLE projects ADD COLUMN uid_base INTEGER", []);
        Ok(Store {
            conn: tokio::sync::Mutex::new(conn),
        })
    }

    pub async fn upsert(&self, id: &Uuid, engine_secret: &str, vault_key: &str) -> Result<()> {
        let c = self.conn.lock().await;
        c.execute(
            "INSERT INTO projects (id, engine_secret, vault_key, desired_running)
             VALUES (?1, ?2, ?3, COALESCE((SELECT desired_running FROM projects WHERE id = ?1), 0))
             ON CONFLICT(id) DO UPDATE SET engine_secret = ?2, vault_key = ?3",
            rusqlite::params![id.to_string(), engine_secret, vault_key],
        )?;
        Ok(())
    }

    pub async fn get(&self, id: &Uuid) -> Result<Option<ProjectRecord>> {
        let c = self.conn.lock().await;
        let mut stmt =
            c.prepare("SELECT engine_secret, vault_key, uid_base FROM projects WHERE id = ?1")?;
        let mut rows = stmt.query(rusqlite::params![id.to_string()])?;
        match rows.next()? {
            None => Ok(None),
            Some(r) => Ok(Some(ProjectRecord {
                id: *id,
                uid_base: r.get::<_, Option<i64>>(2)?.map(|v| v as u32),
                engine_secret: r.get(0)?,
                vault_key: r.get(1)?,
            })),
        }
    }

    /// Allocate this project's uid range, or return the one it already has.
    ///
    /// Two properties matter, and both are about not letting one tenant inherit another's identity:
    ///
    /// * **Never recycled while the row exists.** A uid is a filesystem identity. Handing a
    ///   freed uid to a new project would give it ownership of any stray file the old one left
    ///   behind, so allocation is sticky for the life of the project row.
    /// * **Allocated under one transaction.** `max(uid_base) + stride` read separately from the
    ///   insert is a race: two concurrent provisions would compute the same base and one would
    ///   silently win, leaving two projects sharing a uid. The UNIQUE constraint is the backstop,
    ///   but the transaction is what makes it not happen.
    ///
    /// Each project owns `stride` consecutive uids: the engine runs at `base`, and per-node
    /// children get `base + 1 ..= base + stride - 1` (ADVERSARY F007 — the isolation boundary is
    /// the node, not the project).
    pub async fn allocate_uid(&self, id: &Uuid, range_start: u32, stride: u32) -> Result<u32> {
        anyhow::ensure!(stride > 0, "uid stride must be at least 1");
        let c = self.conn.lock().await;
        let tx = c.unchecked_transaction()?;

        let existing: Option<i64> = tx
            .query_row(
                "SELECT uid_base FROM projects WHERE id = ?1",
                rusqlite::params![id.to_string()],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        if let Some(base) = existing {
            return Ok(base as u32);
        }

        let highest: Option<i64> =
            tx.query_row("SELECT max(uid_base) FROM projects", [], |r| r.get(0))?;
        let base = match highest {
            None => range_start,
            Some(h) => (h as u32)
                .checked_add(stride)
                .context("uid range exhausted")?,
        };

        let updated = tx.execute(
            "UPDATE projects SET uid_base = ?2 WHERE id = ?1",
            rusqlite::params![id.to_string(), base as i64],
        )?;
        anyhow::ensure!(updated == 1, "cannot allocate a uid for an unknown project");
        tx.commit()?;
        Ok(base)
    }

    pub async fn set_desired_running(&self, id: &Uuid, running: bool) -> Result<()> {
        let c = self.conn.lock().await;
        c.execute(
            "UPDATE projects SET desired_running = ?2 WHERE id = ?1",
            rusqlite::params![id.to_string(), running as i64],
        )?;
        Ok(())
    }

    pub async fn delete(&self, id: &Uuid) -> Result<()> {
        let c = self.conn.lock().await;
        c.execute(
            "DELETE FROM projects WHERE id = ?1",
            rusqlite::params![id.to_string()],
        )?;
        Ok(())
    }

    /// Projects that were running when we last knew — the set to restore on boot.
    pub async fn all_desired_running(&self) -> Result<Vec<ProjectRecord>> {
        let c = self.conn.lock().await;
        let mut stmt = c.prepare(
            "SELECT id, engine_secret, vault_key FROM projects WHERE desired_running = 1",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, engine_secret, vault_key) = row?;
            if let Ok(id) = Uuid::parse_str(&id) {
                out.push(ProjectRecord {
                    id,
                    uid_base: None,
                    engine_secret,
                    vault_key,
                });
            }
        }
        Ok(out)
    }
}
