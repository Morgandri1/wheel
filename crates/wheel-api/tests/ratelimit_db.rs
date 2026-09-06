//! The ingress rate limiter.
//!
//! This is the only abuse control in front of an unauthenticated public route, and it counts in the
//! database rather than in process memory precisely so that it does not weaken as replicas scale.
//! These tests therefore need a real database — an in-memory stand-in would test the thing we
//! deliberately did not build.
//!
//! Every case runs against BOTH backends. The limiter is one of the two places where the SQL is
//! genuinely written per dialect — Postgres derives the window boundary from the server clock so
//! that replicas agree on it, SQLite has no `date_trunc` and derives it in-process — so passing on
//! Postgres says nothing about the local install a contributor actually runs. SQLite needs nothing
//! and always runs; Postgres joins it wherever TEST_DATABASE_URL is set.

use uuid::Uuid;
use wheel_api::http::ratelimit::{sweep, RateLimiter};

/// A SQLite store in a fresh temporary file. Always available, so the parity half of every case
/// below runs everywhere — including on a laptop with no Postgres.
async fn sqlite() -> wheel_api::db::Db {
    let path = std::env::temp_dir().join(format!("wheel-rl-{}.db", Uuid::new_v4()));
    wheel_api::db::Db::connect(&format!("sqlite://{}", path.display()))
        .await
        .expect("open a sqlite store")
}

/// Every backend available in this environment.
async fn backends() -> Vec<wheel_api::db::Db> {
    let mut all = vec![sqlite().await];
    all.extend(db().await);
    all
}

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

#[tokio::test]
async fn requests_under_the_limit_are_allowed() {
    for db in backends().await {
        let limiter = RateLimiter::new(5);
        let id = Uuid::new_v4();
        for i in 1..=5 {
            limiter
                .check(&db, &id)
                .await
                .unwrap_or_else(|_| panic!("request {i} of 5 should be inside the budget"));
        }
    }
}

#[tokio::test]
async fn the_request_past_the_limit_is_refused() {
    for db in backends().await {
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
}

#[tokio::test]
async fn budgets_are_per_project() {
    for db in backends().await {
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
}

#[tokio::test]
async fn a_zero_limit_disables_the_control() {
    for db in backends().await {
        let limiter = RateLimiter::new(0);
        let id = Uuid::new_v4();
        for _ in 0..50 {
            limiter.check(&db, &id).await.expect("0 means unlimited");
        }
    }
}

#[tokio::test]
async fn sweep_drops_closed_windows_and_spares_the_current_one() {
    for db in backends().await {
        let limiter = RateLimiter::new(10);
        let id = Uuid::new_v4();
        limiter
            .check(&db, &id)
            .await
            .expect("seed a current-window row");

        // A row from well outside the retention horizon. The instant is bound rather than written
        // as SQL, so the same statement runs on both backends.
        let stale = Uuid::new_v4();
        let stale_window = chrono::Utc::now() - chrono::Duration::hours(2);
        wheel_api::db_execute!(
            &db,
            "INSERT INTO ingress_rate_limits (project_id, window_start, hits) \
             VALUES ($1, $2, 99)",
            stale,
            stale_window
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
}
