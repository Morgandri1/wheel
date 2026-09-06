//! The SQLite schema, applied for real.
//!
//! A migration that parses is not a migration that works: the constraints below are the ones the
//! Postgres schema relies on, and each is load-bearing rather than decorative.

use wheel_api::db::{Db, Dialect};

fn temp_db() -> String {
    let p = std::env::temp_dir().join(format!("wheel-sqlite-{}.db", uuid::Uuid::new_v4()));
    format!("sqlite://{}", p.display())
}

#[tokio::test]
async fn the_sqlite_schema_applies_and_reports_its_dialect() {
    let db = Db::connect(&temp_db()).await.expect("connect and migrate");
    assert_eq!(db.dialect(), Dialect::Sqlite);
}

/// Migrations run on every boot, and a local install boots constantly.
#[tokio::test]
async fn migrating_twice_is_not_an_error() {
    let url = temp_db();
    Db::connect(&url).await.expect("first");
    Db::connect(&url)
        .await
        .expect("second run over the same file");
}

/// A local install's first run has no file. Failing here would mean telling the user to create a
/// database before they can create anything else.
#[tokio::test]
async fn the_database_file_is_created_on_first_use() {
    let path = std::env::temp_dir().join(format!("wheel-fresh-{}.db", uuid::Uuid::new_v4()));
    assert!(!path.exists());
    Db::connect(&format!("sqlite://{}", path.display()))
        .await
        .expect("create on first use");
    assert!(path.exists(), "no database file was created");
}

/// The citext substitute. Without it, address casing silently creates a second account that cannot
/// see the first one's projects — the failure is invisible until a user cannot find their work.
#[tokio::test]
async fn email_uniqueness_ignores_case() {
    let db = Db::connect(&temp_db()).await.unwrap();
    let Db::Sqlite(pool) = &db else {
        panic!("expected sqlite")
    };

    sqlx::query("INSERT INTO users (id, email, password_hash, created_at) VALUES ($1,$2,$3,$4)")
        .bind(uuid::Uuid::new_v4())
        .bind("Alice@Example.com")
        .bind("hash")
        .bind(chrono::Utc::now())
        .execute(pool)
        .await
        .unwrap();

    let duplicate = sqlx::query(
        "INSERT INTO users (id, email, password_hash, created_at) VALUES ($1,$2,$3,$4)",
    )
    .bind(uuid::Uuid::new_v4())
    .bind("alice@example.com")
    .bind("hash")
    .bind(chrono::Utc::now())
    .execute(pool)
    .await;
    assert!(
        duplicate.is_err(),
        "a differently-cased address created a second account"
    );

    let found: (uuid::Uuid,) = sqlx::query_as("SELECT id FROM users WHERE email = $1")
        .bind("ALICE@EXAMPLE.COM")
        .fetch_one(pool)
        .await
        .expect("lookup must be case-insensitive too, not just the index");
    let _ = found;
}

/// `sessions` depends on ON DELETE CASCADE to make deleting a user end their sessions. SQLite
/// leaves foreign keys OFF by default, so this proves the pragma is actually being set.
#[tokio::test]
async fn deleting_a_user_cascades_to_their_sessions() {
    let db = Db::connect(&temp_db()).await.unwrap();
    let Db::Sqlite(pool) = &db else {
        panic!("expected sqlite")
    };
    let user = uuid::Uuid::new_v4();

    sqlx::query("INSERT INTO users (id, email, password_hash, created_at) VALUES ($1,$2,$3,$4)")
        .bind(user)
        .bind("a@b.c")
        .bind("hash")
        .bind(chrono::Utc::now())
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO sessions (id, user_id, created_at, expires_at) VALUES ($1,$2,$3,$4)")
        .bind(uuid::Uuid::new_v4())
        .bind(user)
        .bind(chrono::Utc::now())
        .bind(chrono::Utc::now())
        .execute(pool)
        .await
        .unwrap();

    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(user)
        .execute(pool)
        .await
        .unwrap();

    let left: (i64,) = sqlx::query_as("SELECT count(*) FROM sessions WHERE user_id = $1")
        .bind(user)
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(
        left.0, 0,
        "sessions outlived their user — foreign keys are off"
    );
}

/// Capabilities are a json document in Postgres and text here; the value must survive the trip
/// either way, because `{"http": true}` is what opens a project's public ingress.
#[tokio::test]
async fn project_capabilities_round_trip_as_json() {
    let db = Db::connect(&temp_db()).await.unwrap();
    let Db::Sqlite(pool) = &db else {
        panic!("expected sqlite")
    };
    let id = uuid::Uuid::new_v4();

    sqlx::query("INSERT INTO projects (id, owner_id, name, capabilities) VALUES ($1,$2,$3,$4)")
        .bind(id)
        .bind("user_1")
        .bind("demo")
        .bind(serde_json::json!({"http": true}))
        .execute(pool)
        .await
        .unwrap();

    let (caps,): (serde_json::Value,) =
        sqlx::query_as("SELECT capabilities FROM projects WHERE id = $1")
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap();
    assert_eq!(caps["http"], serde_json::json!(true));
}

/// Ingress is opt-in, and the default lives in the schema. A default that differs between backends
/// would mean a project is publicly reachable on one and not the other.
#[tokio::test]
async fn ingress_is_disabled_by_default() {
    let db = Db::connect(&temp_db()).await.unwrap();
    let Db::Sqlite(pool) = &db else {
        panic!("expected sqlite")
    };
    let id = uuid::Uuid::new_v4();
    sqlx::query("INSERT INTO projects (id, owner_id, name) VALUES ($1,$2,$3)")
        .bind(id)
        .bind("user_1")
        .bind("demo")
        .execute(pool)
        .await
        .unwrap();

    let (caps,): (serde_json::Value,) =
        sqlx::query_as("SELECT capabilities FROM projects WHERE id = $1")
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap();
    assert_eq!(caps["http"], serde_json::json!(false));
}
