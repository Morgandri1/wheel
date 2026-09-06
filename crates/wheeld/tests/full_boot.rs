//! `wheeld` booting the whole product in one process.
//!
//! Everything else about wheeld can be tested without a database; this is the one thing that
//! cannot, and it is also the claim on the tin — one executable, `wheeld`, and Wheel is running.
//! Skipped when there is no Postgres to point at, and a hard failure in CI, where there is one, so
//! "skipped" can never quietly become "never ran".
//!
//! The SQLite store (M1.7 step 2) removes the dependency; until then this is what proves the
//! composition boots for real.

use std::time::Duration;
use uuid::Uuid;

/// `None` means there is no database and the test should be skipped — unless we are in CI, where a
/// missing database is a broken pipeline rather than a reason to pass.
fn database_url() -> Option<String> {
    match std::env::var("TEST_DATABASE_URL") {
        Ok(u) => Some(u),
        Err(_) if std::env::var("WHEEL_CI_HAS_DB").as_deref() == Ok("1") => {
            panic!("WHEEL_CI_HAS_DB=1 but TEST_DATABASE_URL is unset")
        }
        Err(_) => {
            eprintln!("skipping {}: TEST_DATABASE_URL not set", module_path!());
            None
        }
    }
}

#[tokio::test]
async fn wheeld_boots_the_whole_product_in_one_process() {
    let Some(url) = database_url() else { return };

    // wheeld reads its API configuration from the environment it composes, and DATABASE_URL is the
    // one thing it cannot invent.
    std::env::set_var("DATABASE_URL", &url);

    let dir = std::path::PathBuf::from(format!(
        "/tmp/wd-b-{}",
        &Uuid::new_v4().simple().to_string()[..8]
    ));
    let port = {
        // Ask the OS for a free port, then let it go: binding it here and passing the number is
        // racy in principle but far more predictable than guessing a constant in CI.
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let bind = format!("127.0.0.1:{port}");

    let settings = wheeld::config::Settings {
        data_dir: dir.clone(),
        bind: bind.clone(),
    };
    let server = tokio::spawn(async move { wheeld::run(settings).await });

    // Wait for the API to answer rather than sleeping a guess.
    let http = reqwest::Client::new();
    let mut ready = false;
    for _ in 0..100 {
        if server.is_finished() {
            let e = server.await.unwrap().unwrap_err();
            panic!("wheeld exited during boot: {e:#}");
        }
        if let Ok(r) = http.get(format!("http://{bind}/healthz")).send().await {
            if r.status().is_success() {
                ready = true;
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(ready, "wheeld never started serving on {bind}");

    // The API is up; the sandbox host is up alongside it in the same process. A signup proves the
    // database half is wired, which is the part that needed Postgres.
    let email = format!("wheeld-{}@example.test", Uuid::new_v4().simple());
    let signup = http
        .post(format!("http://{bind}/v1/auth/signup"))
        .json(&serde_json::json!({"email": email, "password": "wheeld-test-Passw0rd!"}))
        .send()
        .await
        .expect("signup should reach the API");
    assert_eq!(
        signup.status(),
        201,
        "signup failed: {}",
        signup.text().await.unwrap_or_default()
    );

    server.abort();
}
