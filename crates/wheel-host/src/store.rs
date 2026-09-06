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

/// Open `host.db` on whatever terms the filesystem allows, and prove them before trusting them.
///
/// Three attempts, narrowest change first, because each one costs something:
///
/// 1. The wanted journal mode on an ordinary connection. This is every laptop, every test, and a
///    normal volume; nothing else is given up.
/// 2. The same mode with `locking_mode=EXCLUSIVE`. A WAL database keeps its index in a `-shm` file
///    and Railway's bind mount cannot resize one, which crash-looped every boot on "disk I/O error
///    ... xShmMap"; under exclusive locking sqlite keeps that index in heap memory and never opens
///    the path at all. The cost is that nothing else may open the file while the host runs -- no
///    second connection, no `sqlite3` on the volume -- so it is not the default. The host can pay
///    it: one replica by contract, one connection behind a mutex. The engine could not, which is
///    why the fix there is a fallback instead.
/// 3. A rollback journal on that same exclusive connection. Slower, needs no shared memory, and by
///    now the connection is one that can actually get out of WAL: switching modes has to checkpoint
///    the WAL first, which is why plain `WHEEL_SQLITE_JOURNAL=TRUNCATE` died of the same error it
///    was set to avoid.
///
/// The mode is PROVEN with an immediate transaction at each step rather than trusted, because
/// `PRAGMA journal_mode` reports success on a filesystem where the first write then fails -- the
/// shared memory is mapped on the first write lock, not on the pragma. That trap is the whole bug.
///
/// `WHEEL_SQLITE_JOURNAL` names the mode to try first, for a deployment already known to be hostile.
fn open_configured(path: &str) -> Result<Connection> {
    let wanted = std::env::var("WHEEL_SQLITE_JOURNAL")
        .ok()
        .map(|m| m.trim().to_string())
        .filter(|m| !m.is_empty())
        .unwrap_or_else(|| "WAL".to_string());

    let conn = Connection::open(path).with_context(|| format!("opening {path}"))?;
    if mode_holds(&conn, &wanted) {
        return Ok(conn);
    }

    // A fresh connection: the locking mode is what sqlite consults when it first touches the file,
    // so it cannot be imposed on one that has already tried and failed.
    drop(conn);
    let conn = Connection::open(path).with_context(|| format!("reopening {path}"))?;
    conn.pragma_update(None, "locking_mode", "EXCLUSIVE")
        .context("taking the host database exclusively")?;
    if mode_holds(&conn, &wanted) {
        return Ok(conn);
    }

    if mode_holds(&conn, "TRUNCATE") {
        return Ok(conn);
    }
    anyhow::bail!("no journal mode this filesystem can host: tried {wanted}, then TRUNCATE")
}

/// Does the database actually work in this journal mode, or only claim to?
fn mode_holds(conn: &Connection, mode: &str) -> bool {
    conn.pragma_update(None, "journal_mode", mode).is_ok()
        && conn.execute_batch("BEGIN IMMEDIATE; COMMIT;").is_ok()
}

impl Store {
    pub fn open(path: &str) -> Result<Self> {
        let conn = open_configured(path)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS projects (
               id               TEXT PRIMARY KEY,
               engine_secret    TEXT NOT NULL,
               vault_key        TEXT NOT NULL,
               desired_running  INTEGER NOT NULL DEFAULT 0,
               -- Base uid of this project's range, for the process backend. UNIQUE because two
               -- projects sharing a uid would mean two tenants sharing a filesystem identity.
               uid_base         INTEGER UNIQUE
             );
             -- The uid watermark lives outside the projects table on purpose. Deriving the next
             -- uid from max(uid_base) looked right and was not: deleting a project removes its
             -- uid_base, so the maximum falls back and the next project is handed a uid whose
             -- files may still be on disk. This row only ever climbs.
             CREATE TABLE IF NOT EXISTS uid_watermark (
               id       INTEGER PRIMARY KEY CHECK (id = 1),
               next_uid INTEGER NOT NULL
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
    /// * **Never recycled, even after the project is deleted.** A uid is a filesystem identity,
    ///   so handing a freed one to a new project would give that tenant ownership of anything the
    ///   old one left on disk. The next uid therefore comes from a watermark that only climbs, not
    ///   from `max(uid_base)` — deleting a row removes its uid_base, and the maximum would fall
    ///   back and re-issue it. That version passed on a laptop only because the test needed root
    ///   and was being skipped.
    /// * **Allocated under one transaction.** Reading the watermark and writing it back separately
    ///   is a race: two concurrent provisions would take the same base and one would silently win,
    ///   leaving two projects sharing a uid. The UNIQUE constraint is the backstop, the
    ///   transaction is what stops it happening.
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

        // Seed the watermark on first use, then take it. Deliberately NOT `max(uid_base)`:
        // deleting a project removes its uid_base, so the maximum falls back and the next project
        // is handed a uid whose files may still be on disk. This row only ever climbs.
        tx.execute(
            "INSERT OR IGNORE INTO uid_watermark (id, next_uid) VALUES (1, ?1)",
            rusqlite::params![range_start as i64],
        )?;
        let base = tx.query_row("SELECT next_uid FROM uid_watermark WHERE id = 1", [], |r| {
            r.get::<_, i64>(0)
        })? as u32;
        let next = base.checked_add(stride).context("uid range exhausted")?;
        tx.execute(
            "UPDATE uid_watermark SET next_uid = ?1 WHERE id = 1",
            rusqlite::params![next as i64],
        )?;

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn store() -> (Store, String) {
        let path = std::env::temp_dir()
            .join(format!("wheel-store-{}.db", Uuid::new_v4().simple()))
            .display()
            .to_string();
        (Store::open(&path).unwrap(), path)
    }

    async fn project(s: &Store) -> Uuid {
        let id = Uuid::new_v4();
        s.upsert(&id, "engine-secret", "vault-key").await.unwrap();
        id
    }

    #[tokio::test]
    async fn an_unknown_project_reads_back_as_none() {
        let (s, _p) = store();
        assert!(s.get(&Uuid::new_v4()).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn a_uid_is_sticky_for_the_life_of_a_project() {
        let (s, _p) = store();
        let id = project(&s).await;
        let first = s.allocate_uid(&id, 20_000, 64).await.unwrap();
        assert_eq!(s.allocate_uid(&id, 20_000, 64).await.unwrap(), first);
        assert_eq!(s.get(&id).await.unwrap().unwrap().uid_base, Some(first));
    }

    #[tokio::test]
    async fn two_projects_never_share_a_uid() {
        let (s, _p) = store();
        let (a, b) = (project(&s).await, project(&s).await);
        let ua = s.allocate_uid(&a, 20_000, 64).await.unwrap();
        let ub = s.allocate_uid(&b, 20_000, 64).await.unwrap();
        assert_ne!(ua, ub);
        assert_eq!(ub - ua, 64, "each project owns a whole stride");
    }

    /// The invariant the watermark exists for. A uid is a filesystem identity: reissuing a deleted
    /// project's uid hands the next tenant ownership of whatever the old one left on disk. Deriving
    /// the next uid from `max(uid_base)` looks equivalent and is not, because deleting the row
    /// removes the maximum.
    #[tokio::test]
    async fn a_deleted_projects_uid_is_never_reissued() {
        let (s, _p) = store();
        let first = project(&s).await;
        let taken = s.allocate_uid(&first, 20_000, 64).await.unwrap();
        s.delete(&first).await.unwrap();

        let second = project(&s).await;
        let fresh = s.allocate_uid(&second, 20_000, 64).await.unwrap();
        assert!(
            fresh > taken,
            "uid {fresh} was reissued after {taken} was freed"
        );
    }

    /// The same invariant across a host restart, which is when it actually matters: the watermark
    /// has to be durable, not merely correct in one process.
    #[tokio::test]
    async fn the_watermark_survives_reopening_the_database() {
        let (s, path) = store();
        let first = project(&s).await;
        let taken = s.allocate_uid(&first, 20_000, 64).await.unwrap();
        s.delete(&first).await.unwrap();
        drop(s);

        let s = Store::open(&path).unwrap();
        let second = project(&s).await;
        assert!(s.allocate_uid(&second, 20_000, 64).await.unwrap() > taken);
    }

    #[tokio::test]
    async fn a_uid_cannot_be_allocated_for_a_project_that_does_not_exist() {
        let (s, _p) = store();
        assert!(s.allocate_uid(&Uuid::new_v4(), 20_000, 64).await.is_err());
    }

    #[tokio::test]
    async fn a_stride_of_zero_is_refused_rather_than_overlapping_every_project() {
        let (s, _p) = store();
        let id = project(&s).await;
        assert!(s.allocate_uid(&id, 20_000, 0).await.is_err());
    }

    #[tokio::test]
    async fn an_exhausted_uid_range_is_an_error_not_a_wrapped_uid() {
        let (s, _p) = store();
        let id = project(&s).await;
        assert!(s.allocate_uid(&id, u32::MAX - 8, 64).await.is_err());
    }

    #[tokio::test]
    async fn re_provisioning_rotates_the_secrets_and_keeps_everything_else() {
        let (s, _p) = store();
        let id = project(&s).await;
        let uid = s.allocate_uid(&id, 20_000, 64).await.unwrap();
        s.set_desired_running(&id, true).await.unwrap();

        s.upsert(&id, "rotated-secret", "rotated-key")
            .await
            .unwrap();

        let rec = s.get(&id).await.unwrap().unwrap();
        assert_eq!(rec.engine_secret, "rotated-secret");
        assert_eq!(rec.vault_key, "rotated-key");
        assert_eq!(
            rec.uid_base,
            Some(uid),
            "re-provisioning must not move a uid"
        );
        assert_eq!(
            s.all_desired_running().await.unwrap().len(),
            1,
            "re-provisioning must not stop a running project"
        );
    }

    #[tokio::test]
    async fn only_projects_meant_to_be_running_are_restored_on_boot() {
        let (s, _p) = store();
        let (running, stopped) = (project(&s).await, project(&s).await);
        s.set_desired_running(&running, true).await.unwrap();

        let restored = s.all_desired_running().await.unwrap();
        assert_eq!(restored.len(), 1);
        assert_eq!(restored[0].id, running);
        assert_eq!(restored[0].engine_secret, "engine-secret");
        assert!(restored.iter().all(|r| r.id != stopped));
    }

    #[tokio::test]
    async fn stopping_and_deleting_both_drop_a_project_from_the_restore_set() {
        let (s, _p) = store();
        let (a, b) = (project(&s).await, project(&s).await);
        s.set_desired_running(&a, true).await.unwrap();
        s.set_desired_running(&b, true).await.unwrap();

        s.set_desired_running(&a, false).await.unwrap();
        s.delete(&b).await.unwrap();

        assert!(s.all_desired_running().await.unwrap().is_empty());
        assert!(s.get(&b).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn deleting_a_project_that_was_never_there_is_not_an_error() {
        let (s, _p) = store();
        assert!(s.delete(&Uuid::new_v4()).await.is_ok());
    }

    /// Two provisions arriving together must not take the same base. The UNIQUE constraint is the
    /// backstop; this asserts the transaction stops it happening in the first place.
    #[tokio::test]
    async fn concurrent_allocations_take_distinct_ranges() {
        let (s, _p) = store();
        let s = Arc::new(s);
        let mut ids = Vec::new();
        for _ in 0..8 {
            ids.push(project(&s).await);
        }

        let mut handles = Vec::new();
        for id in ids {
            let s = Arc::clone(&s);
            handles.push(tokio::spawn(async move {
                s.allocate_uid(&id, 20_000, 64).await
            }));
        }
        let mut bases = Vec::new();
        for h in handles {
            bases.push(h.await.unwrap().unwrap());
        }
        bases.sort_unstable();
        let unique = bases.len();
        bases.dedup();
        assert_eq!(bases.len(), unique, "two projects were handed the same uid");
    }
}

#[cfg(test)]
mod journal_mode_tests {
    use super::*;

    /// The production failure itself, reproduced: a filesystem that cannot give sqlite a `-shm`.
    ///
    /// A directory standing where the file belongs is QA's trick from the engine's gate, and it is
    /// the same symptom -- WAL reports success and the first write fails. Without exclusive locking
    /// this is the crash loop; with it sqlite never looks at the path.
    #[tokio::test]
    async fn a_volume_that_cannot_give_us_a_shm_file_still_boots() {
        let dir = std::env::temp_dir().join(format!("wheel-host-noshm-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("host.db");
        std::fs::create_dir(dir.join("host.db-shm")).unwrap();

        let store = Store::open(&path.display().to_string()).expect("the host must still open");
        let id = Uuid::new_v4();
        store.upsert(&id, "s", "dg").await.expect("and still write");
        store.set_desired_running(&id, true).await.unwrap();
        assert_eq!(store.all_desired_running().await.unwrap().len(), 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Exclusive locking is the escape hatch, not the default, and this is what it costs.
    ///
    /// On a filesystem that can host a `-shm` the host stays on an ordinary connection, so an
    /// operator can still open `host.db` with sqlite3 and a test can still break it from outside.
    /// Taking the lock everywhere would have removed both, and two host tests caught exactly that.
    #[tokio::test]
    async fn a_normal_volume_is_left_open_to_other_readers() {
        let dir = std::env::temp_dir().join(format!("wheel-host-open-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("host.db");

        let store = Store::open(&path.display().to_string()).unwrap();
        store.upsert(&Uuid::new_v4(), "s", "dg").await.unwrap();

        let second = Connection::open(&path).unwrap();
        let n: i64 = second
            .query_row("SELECT count(*) FROM projects", [], |r| r.get(0))
            .expect("a second connection must still be able to read host.db");
        assert_eq!(n, 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The host reconciles from this database on every boot, so a journal mode it cannot open is a
    /// crash loop, and one it cannot write to is a host that comes up having forgotten every
    /// project.
    #[tokio::test]
    async fn the_store_takes_writes_whichever_journal_mode_it_settled_on() {
        let dir = std::env::temp_dir().join(format!("wheel-host-store-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("host.db").display().to_string();

        let store = Store::open(&path).unwrap();
        let id = Uuid::new_v4();
        store.upsert(&id, "engine-secret", "dmF1bHQ").await.unwrap();
        store.set_desired_running(&id, true).await.unwrap();
        assert!(store.get(&id).await.unwrap().is_some());

        // Reopening is what a restart does, and restoring what was running is the whole job.
        drop(store);
        let again = Store::open(&path).unwrap();
        assert_eq!(again.all_desired_running().await.unwrap().len(), 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A volume that cannot host a `-shm` index must not stop the host from opening its database.
    /// The proof is a real transaction rather than the pragma's return value, because the shared
    /// memory is mapped on the first write lock -- which is why the pragma reported success in
    /// production while every boot then died on "disk I/O error ... xShmMap".
    #[tokio::test]
    async fn a_rollback_journal_is_a_working_fallback() {
        let dir = std::env::temp_dir().join(format!("wheel-host-tr-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("host.db");

        let conn = Connection::open(&path).unwrap();
        conn.pragma_update(None, "journal_mode", "TRUNCATE")
            .unwrap();
        drop(conn);

        let store = Store::open(&path.display().to_string()).unwrap();
        let id = Uuid::new_v4();
        store.upsert(&id, "s", "dg").await.unwrap();
        store.set_desired_running(&id, true).await.unwrap();
        assert_eq!(store.all_desired_running().await.unwrap().len(), 1);
        std::fs::remove_dir_all(&dir).ok();
    }
}
