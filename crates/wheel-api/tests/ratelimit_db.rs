//! The ingress rate limiter.
//!
//! This is the only control standing in front of an unauthenticated public route, and it counts in
//! Postgres precisely so that it does not weaken as replicas scale. That makes it worth testing
//! against a real database rather than a stub: the semantics under test are the SQL upsert's, not
//! Rust's.

use uuid::Uuid;
use wheel_api::http::ratelimit::{sweep, RateLimiter};

async fn db() -> Option<sqlx::PgPool> {
    let url = match std::env::var("TEST_DATABASE_URL") {
        Ok(u) => u,
        Err(_) if std::env::var("CI").is_ok() => {
            panic!("TEST_DATABASE_URL must be set in CI")
        }
        Err(_) => {
            eprintln!("skipping {}: TEST_DATABASE_URL not set", module_path!());
            return None;
        }
    };
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect(&url)
        .await
        .expect("connect to TEST_DATABASE_URL");
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    Some(pool)
}

#[tokio::test]
async fn allows_up_to_the_limit_then_refuses() {
    let Some(db) = db().await else { return };
    let limiter = RateLimiter::new(5);
    let project = Uuid::new_v4();

    for i in 1..=5 {
        limiter
            .check(&db, &project)
            .await
            .unwrap_or_else(|_| panic!("request {i} of 5 should have been allowed"));
    }
    assert!(
        limiter.check(&db, &project).await.is_err(),
        "the sixth request in a 5/min window should be refused"
    );
}

#[tokio::test]
async fn projects_have_separate_budgets() {
    // A noisy project must not be able to deny service to a quiet one.
    let Some(db) = db().await else { return };
    let limiter = RateLimiter::new(2);
    let (noisy, quiet) = (Uuid::new_v4(), Uuid::new_v4());

    limiter.check(&db, &noisy).await.unwrap();
    limiter.check(&db, &noisy).await.unwrap();
    assert!(limiter.check(&db, &noisy).await.is_err());

    limiter
        .check(&db, &quiet)
        .await
        .expect("one project exhausting its budget must not affect another");
}

#[tokio::test]
async fn a_zero_limit_disables_the_control() {
    // Documented in API.md as the way to turn the limiter off; worth pinning so it cannot regress
    // into "zero requests allowed", which would take the public route offline entirely.
    let Some(db) = db().await else { return };
    let limiter = RateLimiter::new(0);
    let project = Uuid::new_v4();
    for _ in 0..50 {
        limiter
            .check(&db, &project)
            .await
            .expect("0 means unlimited");
    }
}

#[tokio::test]
async fn sweep_removes_only_closed_windows() {
    let Some(db) = db().await else { return };
    let limiter = RateLimiter::new(10);
    let project = Uuid::new_v4();
    limiter.check(&db, &project).await.unwrap();

    // An old window for some other project, well outside the retention interval.
    let stale = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO ingress_rate_limits (project_id, window_start, hits) \
         VALUES ($1, now() - interval '1 hour', 99)",
    )
    .bind(stale)
    .execute(&db)
    .await
    .unwrap();

    sweep(&db).await.unwrap();

    let stale_rows: i64 =
        sqlx::query_scalar("SELECT count(*) FROM ingress_rate_limits WHERE project_id = $1")
            .bind(stale)
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(stale_rows, 0, "closed windows should be swept");

    let live_rows: i64 =
        sqlx::query_scalar("SELECT count(*) FROM ingress_rate_limits WHERE project_id = $1")
            .bind(project)
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(live_rows, 1, "the current window must survive the sweep");
}

#[tokio::test]
async fn concurrent_requests_are_all_counted() {
    // The upsert has to be atomic. If two replicas could read-modify-write, the limit would be
    // exceeded silently under exactly the load it exists to control.
    let Some(db) = db().await else { return };
    let limiter = RateLimiter::new(1000);
    let project = Uuid::new_v4();

    let mut set = tokio::task::JoinSet::new();
    for _ in 0..20 {
        let (db, limiter) = (db.clone(), limiter.clone());
        set.spawn(async move { limiter.check(&db, &project).await.is_ok() });
    }
    while set.join_next().await.is_some() {}

    let hits: i64 = sqlx::query_scalar(
        "SELECT coalesce(sum(hits),0)::bigint FROM ingress_rate_limits WHERE project_id = $1",
    )
    .bind(project)
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(
        hits, 20,
        "every concurrent request must be counted exactly once"
    );
}
