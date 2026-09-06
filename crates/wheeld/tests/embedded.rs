//! The embedded backend against a real engine.
//!
//! These start `wheel_engine::serve` for real, so what they prove is the composition itself: an
//! engine running as a task in this process is reachable on its own unix socket, answers the same
//! control plane, and can be stopped.

use std::time::Duration;
use uuid::Uuid;
use wheel_host::sandbox::{Sandbox, Secrets, Status};
use wheeld::embedded::EmbeddedSandbox;

fn dirs() -> (std::path::PathBuf, std::path::PathBuf) {
    let root = std::env::temp_dir().join(format!("wheeld-{}", Uuid::new_v4().simple()));
    // Short, and outside the data dir: a unix socket path must fit in sun_path (104 bytes on
    // macOS), which a temp dir plus a full uuid twice over does not.
    let run = std::path::PathBuf::from(format!(
        "/tmp/wd-{}",
        &Uuid::new_v4().simple().to_string()[..8]
    ));
    (root, run)
}

fn sandbox() -> EmbeddedSandbox {
    let (data, run) = dirs();
    EmbeddedSandbox::new(data, run, Duration::from_secs(10))
}

fn secrets() -> Secrets {
    Secrets {
        engine_secret: "embedded-engine-secret-16+".into(),
        vault_key: "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=".into(),
    }
}

#[tokio::test]
async fn an_embedded_engine_starts_serves_and_stops() {
    let sb = sandbox();
    let id = Uuid::new_v4();

    assert_eq!(sb.status(&id).await.unwrap(), Status::Stopped);

    sb.start(&id, &secrets())
        .await
        .expect("engine should start");
    assert_eq!(sb.status(&id).await.unwrap(), Status::Running);

    // The socket is the whole interface the host proxies through, so its address must be the unix
    // form the proxy knows how to dial.
    let base = sb.engine_base(&id);
    assert!(base.starts_with("unix://"), "got {base}");
    assert!(sb.socket_path(&id).exists(), "the engine bound no socket");

    sb.stop(&id).await.unwrap();
    assert_eq!(sb.status(&id).await.unwrap(), Status::Stopped);
}

/// A second start must return the engine already running rather than race another onto the same
/// database — the same rule the process backend follows.
#[tokio::test]
async fn starting_twice_does_not_start_a_second_engine() {
    let sb = sandbox();
    let id = Uuid::new_v4();

    sb.start(&id, &secrets()).await.unwrap();
    let first = std::fs::metadata(sb.socket_path(&id)).unwrap();
    sb.start(&id, &secrets()).await.expect("idempotent");
    let second = std::fs::metadata(sb.socket_path(&id)).unwrap();

    assert_eq!(
        first.created().ok(),
        second.created().ok(),
        "the socket was rebound, so a second engine was started"
    );
    assert_eq!(sb.status(&id).await.unwrap(), Status::Running);
    sb.stop(&id).await.unwrap();
}

/// Each project gets its own engine, its own socket and its own database.
#[tokio::test]
async fn projects_do_not_share_an_engine() {
    let sb = sandbox();
    let (a, b) = (Uuid::new_v4(), Uuid::new_v4());

    sb.start(&a, &secrets()).await.unwrap();
    sb.start(&b, &secrets()).await.unwrap();

    assert_ne!(sb.socket_path(&a), sb.socket_path(&b));
    assert_ne!(sb.project_dir(&a), sb.project_dir(&b));
    assert_ne!(sb.engine_base(&a), sb.engine_base(&b));
    assert_eq!(sb.status(&a).await.unwrap(), Status::Running);
    assert_eq!(sb.status(&b).await.unwrap(), Status::Running);

    // Stopping one must not touch the other.
    sb.stop(&a).await.unwrap();
    assert_eq!(sb.status(&a).await.unwrap(), Status::Stopped);
    assert_eq!(sb.status(&b).await.unwrap(), Status::Running);
    sb.stop(&b).await.unwrap();
}

#[tokio::test]
async fn destroy_removes_the_project_and_is_idempotent() {
    let sb = sandbox();
    let id = Uuid::new_v4();

    sb.start(&id, &secrets()).await.unwrap();
    let dir = sb.project_dir(&id);
    assert!(dir.exists());

    sb.destroy(&id).await.unwrap();
    assert!(!dir.exists(), "the project data was left behind");
    assert_eq!(sb.status(&id).await.unwrap(), Status::Stopped);

    sb.destroy(&id).await.expect("destroy is idempotent");
    sb.destroy(&Uuid::new_v4())
        .await
        .expect("destroying an unknown project succeeds");
}

/// A project's directory is its own. The isolation this backend gives up is between the projects of
/// one person; it is not an invitation for other accounts on the machine to read them.
#[cfg(unix)]
#[tokio::test]
async fn project_directories_are_private_to_this_user() {
    use std::os::unix::fs::PermissionsExt;

    let sb = sandbox();
    let id = Uuid::new_v4();
    sb.provision(&id, &secrets()).await.unwrap();

    let mode = std::fs::metadata(sb.project_dir(&id))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o700, "project directory is {mode:o}, want 700");
}

#[tokio::test]
async fn restart_leaves_the_engine_serving() {
    let sb = sandbox();
    let id = Uuid::new_v4();

    sb.start(&id, &secrets()).await.unwrap();
    sb.restart(&id, &secrets()).await.expect("restart");
    assert_eq!(sb.status(&id).await.unwrap(), Status::Running);
    sb.stop(&id).await.unwrap();
}

/// A start that never becomes healthy has to fail, and clean up after itself.
///
/// "Started" must mean "serving": returning Ok as soon as the task is spawned only moves the race
/// into the caller's next request, and the host would report a project as running that answers
/// nothing. A zero timeout is the cheapest way to stand in for an engine that is simply too slow.
#[tokio::test]
async fn a_start_that_never_becomes_healthy_fails_and_leaves_nothing_running() {
    let (data, run) = dirs();
    let sb = EmbeddedSandbox::new(data, run, Duration::ZERO);
    let id = Uuid::new_v4();

    let err = sb
        .start(&id, &secrets())
        .await
        .expect_err("a start that never became healthy reported success");
    assert!(
        format!("{err:#}").contains("did not become healthy"),
        "the reason has to say what did not happen: {err:#}"
    );
    assert_eq!(
        sb.status(&id).await.unwrap(),
        Status::Stopped,
        "a failed start left an engine behind"
    );
}

/// Stopping something that was never started is a success, like every other lifecycle call here.
///
/// The host calls stop on paths where it cannot know whether a start ever happened — a failed
/// reconcile, a delete for a project whose engine died with the last process — and an error there
/// would leave records it refuses to clean up.
#[tokio::test]
async fn stopping_an_engine_that_was_never_started_is_not_an_error() {
    let sb = sandbox();
    let id = Uuid::new_v4();
    sb.stop(&id).await.expect("stop must be idempotent");
    assert_eq!(sb.status(&id).await.unwrap(), Status::Stopped);
}
