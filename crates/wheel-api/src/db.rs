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

/// Which stores this build can actually talk to is a compile-time choice, not only a runtime one.
///
/// A local install runs on SQLite and has no business compiling a Postgres driver: `sqlx-postgres`
/// and its TLS stack are twelve crates a laptop pays for and never loads. The deployed API keeps
/// both, so the same binary can be pointed at either and the dialect parity suite still runs.
#[derive(Clone)]
pub enum Db {
    #[cfg(feature = "postgres")]
    Pg(sqlx::PgPool),
    #[cfg(feature = "sqlite")]
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
        #[cfg(feature = "postgres")]
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

        #[cfg(not(feature = "postgres"))]
        if url.starts_with("postgres://") || url.starts_with("postgresql://") {
            bail!("this build has no Postgres driver: rebuild with the `postgres` feature, or point STORE at a sqlite:// URL");
        }

        #[cfg(feature = "sqlite")]
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

        #[cfg(not(feature = "sqlite"))]
        if sqlite_path(url).is_some() {
            bail!("this build has no SQLite driver: rebuild with the `sqlite` feature, or point STORE at a postgres:// URL");
        }

        bail!(
            "STORE must be a postgres:// or sqlite:// URL, got {:?}",
            scheme_of(url)
        )
    }

    pub fn dialect(&self) -> Dialect {
        match self {
            #[cfg(feature = "postgres")]
            Db::Pg(_) => Dialect::Postgres,
            #[cfg(feature = "sqlite")]
            Db::Sqlite(_) => Dialect::Sqlite,
        }
    }

    /// The Postgres pool, for the few tests that are about Postgres itself rather than about the
    /// API. `Option` rather than a `let ... else`, because in a build with one backend the pattern
    /// is irrefutable and the caller's fallback is dead code.
    #[cfg(feature = "postgres")]
    pub fn as_pg(&self) -> Option<&sqlx::PgPool> {
        match self {
            Db::Pg(pool) => Some(pool),
            #[cfg(feature = "sqlite")]
            _ => None,
        }
    }

    /// The SQLite pool, for the tests that are about SQLite itself.
    #[cfg(feature = "sqlite")]
    pub fn as_sqlite(&self) -> Option<&sqlx::SqlitePool> {
        match self {
            Db::Sqlite(pool) => Some(pool),
            #[cfg(feature = "postgres")]
            _ => None,
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

/// Did this error come from a UNIQUE constraint?
///
/// The two backends report it with different codes — Postgres `23505`, SQLite `2067`/`1555` — and
/// the distinction matters at exactly one place: a duplicate email is a conflict the caller caused,
/// not a server fault, and reporting it as a 500 would tell a user their signup broke when in fact
/// the address is taken.
pub fn is_unique_violation(e: &sqlx::Error) -> bool {
    let sqlx::Error::Database(db) = e else {
        return false;
    };
    matches!(db.code().as_deref(), Some("23505" | "2067" | "1555"))
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

/// Run one expression against whichever pool this build and this configuration provide.
///
/// sqlx is generic over its database, so a single query string and bind list still has to be
/// dispatched once per pool type. Doing that here keeps the cost in one place instead of at every
/// call site, where the two copies would drift.
///
/// It is defined three times, under `cfg`, rather than carrying `cfg` on its arms: a `cfg` inside a
/// macro body is evaluated against the CALLING crate's features, which for `wheeld` would be the
/// wrong answer entirely. On the definition it is evaluated here, where the pools actually exist.
#[cfg(all(feature = "postgres", feature = "sqlite"))]
#[macro_export]
macro_rules! db_dispatch {
    ($db:expr, $pool:ident => $body:expr) => {{
        match $db {
            $crate::db::Db::Pg($pool) => $body,
            $crate::db::Db::Sqlite($pool) => $body,
        }
    }};
}

#[cfg(all(feature = "postgres", not(feature = "sqlite")))]
#[macro_export]
macro_rules! db_dispatch {
    ($db:expr, $pool:ident => $body:expr) => {{
        match $db {
            $crate::db::Db::Pg($pool) => $body,
        }
    }};
}

#[cfg(all(feature = "sqlite", not(feature = "postgres")))]
#[macro_export]
macro_rules! db_dispatch {
    ($db:expr, $pool:ident => $body:expr) => {{
        match $db {
            $crate::db::Db::Sqlite($pool) => $body,
        }
    }};
}

/// Run a statement, returning rows affected.
#[macro_export]
macro_rules! db_execute {
    ($db:expr, $sql:expr $(, $bind:expr)* $(,)?) => {
        $crate::db_dispatch!($db, pool => sqlx::query($sql)$(.bind($bind))*
            .execute(pool).await.map(|r| r.rows_affected()))
    };
}

/// Exactly one row, as a `FromRow` type or a tuple.
#[macro_export]
macro_rules! db_fetch_one {
    ($db:expr, $sql:expr $(, $bind:expr)* $(,)?) => {
        $crate::db_dispatch!($db, pool => sqlx::query_as($sql)$(.bind($bind))*
            .fetch_one(pool).await)
    };
}

/// At most one row.
#[macro_export]
macro_rules! db_fetch_optional {
    ($db:expr, $sql:expr $(, $bind:expr)* $(,)?) => {
        $crate::db_dispatch!($db, pool => sqlx::query_as($sql)$(.bind($bind))*
            .fetch_optional(pool).await)
    };
}

/// Every matching row.
#[macro_export]
macro_rules! db_fetch_all {
    ($db:expr, $sql:expr $(, $bind:expr)* $(,)?) => {
        $crate::db_dispatch!($db, pool => sqlx::query_as($sql)$(.bind($bind))*
            .fetch_all(pool).await)
    };
}

/// A single value from a single row.
#[macro_export]
macro_rules! db_scalar {
    ($db:expr, $sql:expr $(, $bind:expr)* $(,)?) => {
        $crate::db_dispatch!($db, pool => sqlx::query_scalar($sql)$(.bind($bind))*
            .fetch_one(pool).await)
    };
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
        // Matched rather than unwrapped: `Db` deliberately has no `Debug`, because a pool's
        // debug output can carry the connection string, password included.
        let msg = match Db::connect("mysql://localhost/wheel").await {
            Ok(_) => panic!("an unsupported url must not connect"),
            Err(e) => format!("{e:#}"),
        };
        assert!(msg.contains("mysql"), "{msg}");
        assert!(
            msg.contains("sqlite://"),
            "the message should say what is accepted: {msg}"
        );
    }

    // Constructing a pool needs a runtime even when lazy, so this is async rather than a plain
    // `#[test]`.
    #[tokio::test]
    async fn pick_returns_the_statement_for_the_backend_in_use() {
        let sqlite = Db::Sqlite(sqlx::sqlite::SqlitePool::connect_lazy("sqlite::memory:").unwrap());
        assert_eq!(sqlite.dialect(), Dialect::Sqlite);
        assert_eq!(sqlite.pick("PG", "SQLITE"), "SQLITE");
    }
}
