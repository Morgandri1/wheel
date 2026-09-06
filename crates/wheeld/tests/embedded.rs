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
