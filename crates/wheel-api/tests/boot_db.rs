// Postgres-only: it asserts on a `Db::Pg` pool, which a build without the `postgres` feature does
// not have. The SQLite half of the same wiring is `tests/sqlite_store.rs`.
#![cfg(feature = "postgres")]

//! Startup wiring.
//!
//! `main` is now a shell, so what is worth testing is that the pieces it assembles actually work:
//! migrations run against an empty database, the assembled state serves requests, and the periodic
//! sweep reclaims what it should.

mod ws_support;
use std::time::Duration;
use ws_support::{cfg, db_url};

macro_rules! url_or_skip {
    () => {
        match db_url() {
            Some(u) => u,
            None => return,
        }
    };
}

#[tokio::test]
async fn migrations_run_against_an_empty_database() {
    let url = url_or_skip!();
    let cfg = cfg(&url);

    // Idempotent: booting a second replica against a migrated database must be a no-op, not a
    // conflict. Every deploy does exactly this.
    let db = wheel_api::boot::connect_and_migrate(&cfg)
        .await
        .expect("first boot");
    let again = wheel_api::boot::connect_and_migrate(&cfg)
        .await
        .expect("second boot");

    for table in [
        "projects",
        "project_secrets",
        "ws_tickets",
        "ingress_rate_limits",
    ] {
        // information_schema is Postgres's, and this suite only runs against Postgres; the SQLite
        // schema is covered by tests/sqlite_store.rs, which applies it for real.
        let pool = db.as_pg().expect("a postgres store");
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM information_schema.tables WHERE table_name = $1)",
        )
        .bind(table)
        .fetch_one(pool)
        .await
        .unwrap();
        assert!(exists, "{table} should exist after migrations");
    }
    drop(again);
}

#[tokio::test]
async fn the_assembled_state_serves_requests() {
    let url = url_or_skip!();
    let cfg = cfg(&url);
    let http = wheel_api::boot::http_client(&cfg).expect("http client");
    let db = wheel_api::boot::connect_and_migrate(&cfg)
        .await
        .expect("db");

    // JWKS priming fails here (the URL is unreachable by design) and must not prevent startup —
    // a briefly unreachable identity provider should not turn a deploy into an outage.
    let state = wheel_api::boot::build_state(cfg, db, http).await;
    let app = wheel_api::build_router(state, &["https://wheel.dev".to_string()]);

    let resp = tower::ServiceExt::oneshot(
        app,
        axum::http::Request::builder()
            .uri("/healthz")
            .body(axum::body::Body::empty())
            .unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(resp.status(), axum::http::StatusCode::OK);
}

#[tokio::test]
async fn maintenance_reclaims_expired_rows() {
    let url = url_or_skip!();
    let cfg = cfg(&url);
    let db = wheel_api::boot::connect_and_migrate(&cfg)
        .await
        .expect("db");

    let stale_project = uuid::Uuid::new_v4();
    wheel_api::db_execute!(
        &db,
        "INSERT INTO ingress_rate_limits (project_id, window_start, hits) \
         VALUES ($1, now() - interval '3 hours', 1)",
        stale_project
    )
    .unwrap();

    wheel_api::boot::run_maintenance_once(&db).await;

    let left: i64 = wheel_api::db_scalar!(
        &db,
        "SELECT count(*) FROM ingress_rate_limits WHERE project_id = $1",
        stale_project
    )
    .unwrap();
    assert_eq!(
        left, 0,
        "the sweep should have reclaimed a 3-hour-old window"
    );
}

#[tokio::test]
async fn the_maintenance_task_keeps_running() {
    let url = url_or_skip!();
    let cfg = cfg(&url);
    let db = wheel_api::boot::connect_and_migrate(&cfg)
        .await
        .expect("db");

    let handle = wheel_api::boot::spawn_maintenance(db, Duration::from_millis(20));
    tokio::time::sleep(Duration::from_millis(120)).await;
    assert!(
        !handle.is_finished(),
        "the maintenance loop must survive its own iterations, not exit after the first"
    );
    handle.abort();
}

#[tokio::test]
async fn the_orchestrator_is_the_host_client() {
    let url = url_or_skip!();
    let cfg = cfg(&url);
    let http = wheel_api::boot::http_client(&cfg).expect("client");
    let orch = wheel_api::boot::build_orchestrator(&cfg, http);

    // cfg points host_url at an unroutable host, so a call must fail rather than silently succeed
    // against nothing — proof that this is a real client and not a no-op stand-in.
    assert!(orch.status(&uuid::Uuid::new_v4()).await.is_err());
}
