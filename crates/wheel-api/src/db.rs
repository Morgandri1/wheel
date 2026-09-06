//! The database the API runs on: Postgres in production, SQLite for a local install.
//!
//! Postgres remains the deployed store — it is what N replicas share, and the shared counters and
//! ws-ticket expiry are correct there because they are computed from the *database* clock rather
//! than from any replica's. SQLite exists so `wheeld` can be one executable with nothing to install.
//!
//! Most SQL is identical between the two: `$N` placeholders, uuid, `RETURNING` and `ON CONFLICT`
//! all behave the same way (pinned by `tests/sqlite_dialect.rs`). What differs is time arithmetic —
//! `now()`, `make_interval`, `interval '2 hours'` have no SQLite equivalent — and the `citext` and
//! `jsonb` column types. Those are the only places a query is written twice, and `Dialect` is how a
//! call site says so out loud instead of silently working on one backend.

use anyhow::{bail, Context, Result};

#[derive(Clone)]
pub enum Db {
    Pg(sqlx::PgPool),
    Sqlite(sqlx::SqlitePool),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    Postgres,
    Sqlite,
}

impl Db {
    /// Connect and migrate, choosing the backend from the URL scheme.
    ///
    /// The scheme is the whole of the decision, so there is no mode flag that can disagree with the
    /// connection string.
    pub async fn connect(url: &str) -> Result<Self> {
        if url.starts_with("postgres://") || url.starts_with("postgresql://") {
            let pool = sqlx::postgres::PgPoolOptions::new()
                .max_connections(10)
                .connect(url)
                .await
                .context("connecting to postgres")?;
            sqlx::migrate!("./migrations")
                .run(&pool)
                .await
                .context("running postgres migrations")?;
            return Ok(Db::Pg(pool));
        }

        if let Some(path) = sqlite_path(url) {
            // create_if_missing: a local install's first run has no file, and failing there would
            // mean telling the user to create a database before they can create anything else.
            let opts = sqlx::sqlite::SqliteConnectOptions::new()
                .filename(path)
                .create_if_missing(true)
                // WAL so a reader is never blocked by the writer; the engine uses it for the same
                // reason. `foreign_keys` is OFF by default in SQLite, and the sessions table
                // depends on ON DELETE CASCADE, so it has to be asked for explicitly.
                .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
                .foreign_keys(true);
            let pool = sqlx::sqlite::SqlitePoolOptions::new()
                .max_connections(5)
                .connect_with(opts)
                .await
                .context("opening the sqlite database")?;
            sqlx::migrate!("./migrations_sqlite")
                .run(&pool)
                .await
                .context("running sqlite migrations")?;
            return Ok(Db::Sqlite(pool));
        }

        bail!(
            "STORE must be a postgres:// or sqlite:// URL, got {:?}",
            scheme_of(url)
        )
    }

    pub fn dialect(&self) -> Dialect {
        match self {
            Db::Pg(_) => Dialect::Postgres,
            Db::Sqlite(_) => Dialect::Sqlite,
        }
    }

    /// Pick the statement for this backend. Used only where the dialects genuinely differ, so a
    /// call site that needs two queries has to say which is which.
    pub fn pick<'a>(&self, postgres: &'a str, sqlite: &'a str) -> &'a str {
        match self.dialect() {
            Dialect::Postgres => postgres,
            Dialect::Sqlite => sqlite,
        }
    }
}

/// The filesystem path inside a `sqlite:` URL, or `None` if this is not one.
///
/// Accepts the spellings people actually write — `sqlite:x.db`, `sqlite://x.db`,
/// `sqlite:///abs/x.db` — plus `:memory:` for tests.
fn sqlite_path(url: &str) -> Option<String> {
    let rest = url
        .strip_prefix("sqlite://")
        .or_else(|| url.strip_prefix("sqlite:"))?;
    let rest = rest.split('?').next().unwrap_or(rest);
    if rest.is_empty() {
        return None;
    }
    Some(rest.to_string())
}

fn scheme_of(url: &str) -> &str {
    url.split_once("://")
        .map(|(s, _)| s)
        .or_else(|| url.split_once(':').map(|(s, _)| s))
        .unwrap_or(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_url_scheme_decides_the_backend_and_nothing_else_does() {
        assert!(sqlite_path("postgres://u:p@host/db").is_none());
        assert!(sqlite_path("postgresql://u:p@host/db").is_none());
        assert_eq!(
            sqlite_path("sqlite://wheel.db").as_deref(),
            Some("wheel.db")
        );
        assert_eq!(sqlite_path("sqlite:wheel.db").as_deref(), Some("wheel.db"));
        assert_eq!(
            sqlite_path("sqlite:///var/lib/wheel.db").as_deref(),
            Some("/var/lib/wheel.db")
        );
    }

    /// Query parameters are not part of the path. `sqlite://x.db?mode=rwc` naming a file called
    /// `x.db?mode=rwc` is the kind of thing that only shows up as a mysteriously empty database.
    #[test]
    fn query_parameters_are_not_part_of_the_filename() {
        assert_eq!(
            sqlite_path("sqlite://wheel.db?mode=rwc").as_deref(),
            Some("wheel.db")
        );
    }

    #[test]
    fn a_url_with_no_recognised_scheme_names_what_it_saw() {
        assert_eq!(scheme_of("mysql://host/db"), "mysql");
        assert_eq!(scheme_of("wheel.db"), "wheel.db");
    }

    #[tokio::test]
    async fn an_unsupported_url_is_refused_with_the_scheme_in_the_message() {
        let e = Db::connect("mysql://localhost/wheel").await.unwrap_err();
        let msg = format!("{e:#}");
        assert!(msg.contains("mysql"), "{msg}");
        assert!(
            msg.contains("sqlite://"),
            "the message should say what is accepted: {msg}"
        );
    }

    #[test]
    fn pick_returns_the_statement_for_the_backend_in_use() {
        let sqlite = Db::Sqlite(sqlx::sqlite::SqlitePool::connect_lazy("sqlite::memory:").unwrap());
        assert_eq!(sqlite.dialect(), Dialect::Sqlite);
        assert_eq!(sqlite.pick("PG", "SQLITE"), "SQLITE");
    }
}
