//! The host's own durable state: which projects exist, their secrets, and whether they are
//! *supposed* to be running.
//!
//! This is what makes reconciliation-on-boot possible. The host is a single instance by design, so
//! if it restarts, the only record that project X should be running is here.

use anyhow::{Context, Result};
use rusqlite::Connection;
use uuid::Uuid;

pub struct Store {
    conn: tokio::sync::Mutex<Connection>,
}

/// Note: `desired_running` is not a field here. It is a query predicate — `all_desired_running`
/// filters on it in SQL — and carrying a stale copy around in memory would invite acting on it.
#[derive(Debug, Clone)]
pub struct ProjectRecord {
    pub id: Uuid,
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
               desired_running  INTEGER NOT NULL DEFAULT 0
             );",
        )?;
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
        let mut stmt = c.prepare("SELECT engine_secret, vault_key FROM projects WHERE id = ?1")?;
        let mut rows = stmt.query(rusqlite::params![id.to_string()])?;
        match rows.next()? {
            None => Ok(None),
            Some(r) => Ok(Some(ProjectRecord {
                id: *id,
                engine_secret: r.get(0)?,
                vault_key: r.get(1)?,
            })),
        }
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
                    engine_secret,
                    vault_key,
                });
            }
        }
        Ok(out)
    }
}
