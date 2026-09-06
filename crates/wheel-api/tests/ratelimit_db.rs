//! The ingress rate limiter.
//!
//! This is the only abuse control in front of an unauthenticated public route, and it counts in
//! Postgres rather than in process memory precisely so that it does not weaken as replicas scale.
//! These tests therefore need a real database — an in-memory stand-in would test the thing we
//! deliberately did not build.

use uuid::Uuid;
use wheel_api::http::ratelimit::{sweep, RateLimiter};

async fn db() -> Option<wheel_api::db::Db> {
    let url = match std::env::var("TEST_DATABASE_URL") {
        Ok(u) => u,
        Err(_) if std::env::var("WHEEL_CI_HAS_DB").as_deref() == Ok("1") => {
            panic!("WHEEL_CI_HAS_DB=1 but TEST_DATABASE_URL is unset")
        }
        Err(_) => {
            eprintln!("skipping ratelimit tests: TEST_DATABASE_URL not set");
            return None;
        }
    };
    let pool = wheel_api::db::Db::connect(&url)
        .await
        .expect("connect and migrate");
    Some(pool)
}

macro_rules! db_or_skip {
    () => {
        match db().await {
            Some(p) => p,
            None => return,
        }
    };
}

#[tokio::test]
async fn requests_under_the_limit_are_allowed() {
    let db = db_or_skip!();
    let limiter = RateLimiter::new(5);
    let id = Uuid::new_v4();
    for i in 1..=5 {
        limiter
            .check(&db, &id)
            .await
            .unwrap_or_else(|_| panic!("request {i} of 5 should be inside the budget"));
    }
}

#[tokio::test]
async fn the_request_past_the_limit_is_refused() {
    let db = db_or_skip!();
    let limiter = RateLimiter::new(3);
    let id = Uuid::new_v4();
    for _ in 0..3 {
        limiter.check(&db, &id).await.expect("within budget");
    }
    assert!(
        limiter.check(&db, &id).await.is_err(),
        "the 4th request in a 3/min window must be refused"
    );
    // And it stays refused rather than flapping.
    assert!(limiter.check(&db, &id).await.is_err());
}

#[tokio::test]
async fn budgets_are_per_project() {
    let db = db_or_skip!();
    let limiter = RateLimiter::new(2);
    let noisy = Uuid::new_v4();
    let quiet = Uuid::new_v4();

    for _ in 0..2 {
        limiter.check(&db, &noisy).await.expect("within budget");
    }
    assert!(limiter.check(&db, &noisy).await.is_err());

    // One tenant exhausting its budget must not spend another tenant's.
    limiter
        .check(&db, &quiet)
        .await
        .expect("a different project has its own budget");
}

#[tokio::test]
async fn a_zero_limit_disables_the_control() {
    let db = db_or_skip!();
    let limiter = RateLimiter::new(0);
    let id = Uuid::new_v4();
    for _ in 0..50 {
        limiter.check(&db, &id).await.expect("0 means unlimited");
    }
}

#[tokio::test]
async fn sweep_drops_closed_windows_and_spares_the_current_one() {
    let db = db_or_skip!();
    let limiter = RateLimiter::new(10);
    let id = Uuid::new_v4();
    limiter
        .check(&db, &id)
        .await
        .expect("seed a current-window row");

    // A row from well outside the retention horizon.
    let stale = Uuid::new_v4();
    wheel_api::db_execute!(
        &db,
        "INSERT INTO ingress_rate_limits (project_id, window_start, hits) \
         VALUES ($1, now() - interval '2 hours', 99)",
        stale
    )
    .expect("insert stale row");

    sweep(&db).await.expect("sweep");

    let stale_left: i64 = wheel_api::db_scalar!(
        &db,
        "SELECT count(*) FROM ingress_rate_limits WHERE project_id = $1",
        stale
    )
    .unwrap();
    assert_eq!(stale_left, 0, "closed windows must be reclaimed");

    let current_left: i64 = wheel_api::db_scalar!(
        &db,
        "SELECT count(*) FROM ingress_rate_limits WHERE project_id = $1",
        id
    )
    .unwrap();
    assert_eq!(
        current_left, 1,
        "sweeping must not reset the window a caller is currently being counted against"
    );
}
