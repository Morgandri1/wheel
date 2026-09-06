//! The wheeld composition, driven for real.
//!
//! `run` also stands up the API, which needs Postgres, so this drives the half that does not:
//! `start_host` binds the sandbox host, reconciles it and serves it. What that proves is the whole
//! point of wheeld — that a project's engine, embedded as a task in *this* process, is reachable
//! through the ordinary host API over the ordinary loopback hop the API will use.
//!
//! Deliberately end to end rather than a unit test of the wiring: the failure this guards against
//! is the pieces being individually right and not fitting together.

use uuid::Uuid;
use wheeld::supervise::Keys;

/// One data directory per test. `start_host` writes environment defaults, and those are
/// process-global, so these tests share a process carefully: each uses its own directory and only
/// the first `apply_defaults` wins (it never overrides what is already set), which is the behaviour
/// under test in supervise's own suite.
fn data_dir() -> std::path::PathBuf {
    // Short: the engine's unix socket lives under here and must fit in sun_path.
    let p = std::path::PathBuf::from(format!(
        "/tmp/wd-c-{}",
        &Uuid::new_v4().simple().to_string()[..8]
    ));
    std::fs::create_dir_all(&p).unwrap();
    p
}

async fn start() -> (String, String) {
    let dir = wheeld::supervise::prepare_data_dir(&data_dir()).unwrap();
    let keys = Keys::load_or_create(&dir).unwrap();
    let url = wheeld::start_host(&dir, &keys)
        .await
        .expect("the sandbox host should start");

    // Read the secret back out of the composed environment rather than from `keys`. Defaults are
    // applied without overriding, so in a process that has already booted a host the standing value
    // is the one the host is actually using — and the point of this test is to authenticate the way
    // the API does, from the environment, not from a value we happen to be holding.
    let secret = std::env::var("WHEEL_HOST_SECRET").expect("the host secret should be composed");
    (url, secret)
}

#[tokio::test]
async fn the_embedded_host_serves_and_runs_a_projects_engine() {
    let (host_url, secret) = start().await;
    let http = reqwest::Client::new();

    // Liveness first: the host is listening at all.
    let live = http
        .get(format!("{host_url}/healthz"))
        .send()
        .await
        .expect("the host should be listening");
    assert_eq!(live.status(), 200);

    // Provision and start a project, exactly as the API would.
    let id = Uuid::new_v4();
    let put = http
        .put(format!("{host_url}/host/v1/projects/{id}"))
        .bearer_auth(&secret)
        .json(&serde_json::json!({
            "engine_secret": "composition-engine-secret-16+",
            "vault_key": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(put.status(), 200, "provision failed");

    let start = http
        .post(format!("{host_url}/host/v1/projects/{id}/start"))
        .bearer_auth(&secret)
        .send()
        .await
        .unwrap();
    assert_eq!(
        start.status(),
        200,
        "the embedded engine did not start: {}",
        start.text().await.unwrap_or_default()
    );

    // The engine is embedded in this very process, and the host reports it running.
    let status: serde_json::Value = http
        .get(format!("{host_url}/host/v1/projects/{id}"))
        .bearer_auth(&secret)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(status["status"], "running", "got {status}");

    // And the host proxies to it: this is the path the API's engine proxy takes, over the unix
    // socket the embedded engine bound. A board on a fresh project is empty but must answer.
    let board = http
        .get(format!("{host_url}/host/v1/projects/{id}/engine/v1/board"))
        .bearer_auth(&secret)
        .send()
        .await
        .unwrap();
    assert_eq!(
        board.status(),
        200,
        "the host could not reach the embedded engine"
    );
    let board: serde_json::Value = board.json().await.unwrap();
    assert!(board["nodes"].is_array(), "got {board}");

    // Tear it down through the same API.
    let stop = http
        .post(format!("{host_url}/host/v1/projects/{id}/stop"))
        .bearer_auth(&secret)
        .send()
        .await
        .unwrap();
    assert_eq!(stop.status(), 200);
}

/// The host secret is generated per boot and is the only thing standing between a local process and
/// every project on the machine. An unauthenticated caller must get nothing.
#[tokio::test]
async fn the_embedded_host_still_requires_its_bearer() {
    let (host_url, _) = start().await;
    let http = reqwest::Client::new();

    let denied = http
        .get(format!("{host_url}/host/v1/projects/{}", Uuid::new_v4()))
        .send()
        .await
        .unwrap();
    assert_eq!(denied.status(), 401);
}

/// `--help` and `--version` must print and exit cleanly.
///
/// They are what someone runs first, often before anything is configured, so neither may start a
/// server, touch a data directory, or fail because the environment is not set up yet.
#[tokio::test]
async fn help_and_version_do_their_job_without_starting_anything() {
    use wheeld::config::{Action, Settings};

    let before = std::env::var("WHEEL_HOST_SECRET").ok();

    wheeld::dispatch(Settings::parse(["--help"]).unwrap())
        .await
        .expect("--help must succeed");
    wheeld::dispatch(Settings::parse(["--version"]).unwrap())
        .await
        .expect("--version must succeed");

    assert_eq!(
        std::env::var("WHEEL_HOST_SECRET").ok(),
        before,
        "printing usage must not compose an environment"
    );
    assert!(matches!(
        Settings::parse(["--help"]).unwrap(),
        Action::PrintUsage
    ));
}

/// `cli_main` is the whole binary. Driving it here means `main` is a call with nothing in it to go
/// wrong, and that the usage path works end to end including building a runtime.
#[test]
fn the_binary_entry_point_handles_usage_and_rejects_a_bad_flag() {
    wheeld::cli_main(["--help"]).expect("--help");
    wheeld::cli_main(["--version"]).expect("--version");

    let e = wheeld::cli_main(["--nonsense"]).expect_err("an unknown flag must not run anything");
    assert!(format!("{e:#}").contains("--nonsense"));
}
