//! The SQLite behaviours the dual-dialect store is built on.
//!
//! `STORE=sqlite://` exists so a local install needs no Postgres. Sharing one set of SQL between
//! the two dialects is only safe because of the five properties below; each was verified rather
//! than assumed, and each would fail somewhere far from its cause if it stopped holding — a uuid
//! that does not round-trip is a lookup that silently finds nothing, and a collation that stops
//! being case-insensitive is two accounts for one email address.
//!
//! What is NOT shared, and why the store still branches: Postgres time arithmetic (`now()`,
//! `make_interval`, `interval '5 minutes'`) has no SQLite equivalent, and `citext`/`jsonb` are
//! column types rather than expressions.
// Exercises the SQLite backend, so it exists only in a build that has one.
#![cfg(feature = "sqlite")]

use sqlx::sqlite::SqlitePoolOptions;
use uuid::Uuid;

#[tokio::test]
async fn the_dialect_assumptions_behind_a_shared_query_still_hold() {
    let pool = SqlitePoolOptions::new()
        .connect("sqlite::memory:")
        .await
        .unwrap();

    sqlx::query(
        "CREATE TABLE t (id TEXT PRIMARY KEY, email TEXT COLLATE NOCASE UNIQUE, \
         created_at TEXT NOT NULL, n INTEGER)",
    )
    .execute(&pool)
    .await
    .unwrap();

    let id = Uuid::new_v4();
    // 1. Does the $1 placeholder style work?
    let dollar = sqlx::query("INSERT INTO t (id, email, created_at, n) VALUES ($1, $2, $3, $4)")
        .bind(id)
        .bind("Alice@Example.com")
        .bind(chrono::Utc::now())
        .bind(1i64)
        .execute(&pool)
        .await;
    assert!(
        dollar.is_ok(),
        "$N placeholders are what let one query string serve both dialects: {dollar:?}"
    );

    // 2. Does a Uuid round-trip?
    let back: (Uuid,) = sqlx::query_as("SELECT id FROM t WHERE id = $1")
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(back.0, id, "uuid did not round-trip");

    // 3. Is COLLATE NOCASE a workable citext stand-in?
    // The stand-in for Postgres citext. Without it, address casing silently creates a second
    // account that cannot see the first one's projects.
    let found: (String,) = sqlx::query_as("SELECT email FROM t WHERE email = $1")
        .bind("alice@example.com")
        .fetch_one(&pool)
        .await
        .expect("COLLATE NOCASE must match a different casing, as citext does");
    assert_eq!(found.0, "Alice@Example.com");

    // 4. Does RETURNING work?
    let ret: (i64,) = sqlx::query_as("UPDATE t SET n = n + 1 WHERE id = $1 RETURNING n")
        .bind(id)
        .fetch_one(&pool)
        .await
        .expect("RETURNING should be supported");
    assert_eq!(ret.0, 2);

    // 5. Do timestamps round-trip as chrono?
    let before = chrono::Utc::now();
    let ts: (chrono::DateTime<chrono::Utc>,) = sqlx::query_as("SELECT created_at FROM t")
        .fetch_one(&pool)
        .await
        .expect("timestamps must round-trip: expiry checks depend on it");
    assert!(
        (before - ts.0).num_seconds().abs() < 60,
        "a timestamp came back as a different instant: {} vs {before}",
        ts.0
    );

    // 6. ON CONFLICT upsert, used by the shared rate limiters.
    sqlx::query(
        "INSERT INTO t (id, email, created_at, n) VALUES ($1, $2, $3, 1) \
                 ON CONFLICT(id) DO UPDATE SET n = n + 1",
    )
    .bind(id)
    .bind("other@example.com")
    .bind(chrono::Utc::now())
    .execute(&pool)
    .await
    .expect("ON CONFLICT upsert is how the shared rate limiters count");
}
