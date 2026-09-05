//! The process sandbox backend.
//!
//! The privilege drop itself needs root to exercise, and CI does not have it. What these cover is
//! everything that is observable without it: the paths, the modes, and — most importantly — the
//! properties whose violation would be silent. A TCP endpoint creeping into `engine_base`, or a
//! socket landing outside a 0700 directory, would not fail any build; it would just quietly stop
//! isolating tenants.

use std::sync::Arc;
use uuid::Uuid;
use wheel_host::config::{Backend, Config};
use wheel_host::sandbox::process::ProcessSandbox;
use wheel_host::sandbox::{Sandbox, Secrets, Status};
use wheel_host::store::Store;

fn scratch() -> String {
    let d = std::env::temp_dir().join(format!("wheel-proc-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&d).unwrap();
    d.to_str().unwrap().to_string()
}

fn cfg(data_dir: &str, run_dir: &str) -> Config {
    Config {
        bind_addr: "127.0.0.1:0".into(),
        secret: "host-secret-at-least-16-chars".into(),
        backend: Backend::Process,
        data_dir: data_dir.into(),
        engine_image: "unused".into(),
        docker_network: "unused".into(),
        engine_port: 7000,
        memory_bytes: 1 << 30,
        nano_cpus: 1_000_000_000,
        pids_limit: 512,
        start_timeout_secs: 2,
        uid_range_start: 20_000,
        uid_stride: 64,
        run_dir: run_dir.into(),
        engine_base_url: "unused".into(),
    }
}

fn sandbox() -> (ProcessSandbox, Arc<Store>, String, String) {
    let data = scratch();
    let run = scratch();
    let store = Arc::new(Store::open(&format!("{data}/host.db")).unwrap());
    (
        ProcessSandbox::new(cfg(&data, &run), store.clone()),
        store,
        data,
        run,
    )
}

/// Directory ownership is the whole mechanism, and only root can hand a directory to another uid.
/// Skipped off-root so a laptop run stays useful; hard-failed where root was promised, so this
/// cannot quietly stop covering the thing it exists to cover.
macro_rules! require_root {
    () => {
        #[cfg(unix)]
        {
            let is_root = unsafe { libc::geteuid() } == 0;
            if !is_root {
                if std::env::var("WHEEL_CI_HAS_ROOT").as_deref() == Ok("1") {
                    panic!("WHEEL_CI_HAS_ROOT=1 but this process is not root");
                }
                eprintln!("skipping: the process backend needs root to chown project directories");
                return;
            }
        }
    };
}

fn secrets() -> Secrets {
    Secrets {
        engine_secret: "engine-secret-at-least-16-chars".into(),
        vault_key: "dmF1bHQta2V5".into(),
    }
}

#[tokio::test]
async fn engine_is_addressed_by_pathname_socket_never_tcp() {
    // Two failures this rules out. A TCP endpoint would be reachable by every other tenant on a
    // shared kernel. An abstract socket (a leading NUL, rendered as `@`) ignores filesystem
    // permissions entirely, so any uid could connect regardless of directory mode.
    let (s, _store, _d, _r) = sandbox();
    let id = Uuid::new_v4();
    let base = s.engine_base(&id);

    assert!(base.starts_with("unix://"), "got {base}");
    assert!(base.ends_with("engine.sock"), "got {base}");
    assert!(
        !base.contains("127.0.0.1") && !base.contains("0.0.0.0"),
        "got {base}"
    );
    assert!(
        !base.contains("://@") && !base.contains('\0'),
        "abstract-namespace socket: filesystem permissions would not apply — {base}"
    );
}

#[tokio::test]
async fn provision_creates_private_dirs_and_is_idempotent() {
    require_root!();
    let (s, store, _d, _r) = sandbox();
    let id = Uuid::new_v4();
    // provision allocates this project's uid, so the row has to exist — the same order the host
    // uses in put_project.
    store.upsert(&id, "s", "v").await.unwrap();

    s.provision(&id, &secrets()).await.expect("provision");
    s.provision(&id, &secrets())
        .await
        .expect("provision is called on every PUT; must be a no-op");

    for dir in [s.project_dir(&id), s.tmp_dir(&id), s.run_dir(&id)] {
        assert!(dir.is_dir(), "{} was not created", dir.display());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o700, "{} is {mode:o}, must be 0700", dir.display());
        }
    }
}

#[tokio::test]
async fn each_project_gets_a_private_tmpdir_inside_its_own_tree() {
    // A shared /tmp is a cross-tenant channel: predictable names and readable metadata. The temp
    // directory has to live inside the project's 0700 tree, not beside it.
    let (s, _store, _d, _r) = sandbox();
    let a = Uuid::new_v4();
    let b = Uuid::new_v4();

    // Inside the project's own 0700 tree, and distinct per project. Those two are the property.
    assert!(s.tmp_dir(&a).starts_with(s.project_dir(&a)));
    assert_ne!(s.tmp_dir(&a), s.tmp_dir(&b));

    // Not the process-wide temp directory. Checked against `env::temp_dir()` rather than the
    // literal "/tmp": on Linux `env::temp_dir()` *is* /tmp, so this test's own scratch directory
    // lives under it, and a literal check failed in CI for a reason that had nothing to do with
    // the code — it was asserting something about the fixture, not about the sandbox.
    assert_ne!(s.tmp_dir(&a), std::env::temp_dir());
}

#[tokio::test]
async fn projects_never_share_a_directory_or_a_uid() {
    require_root!();
    let (s, store, _d, _r) = sandbox();
    let a = Uuid::new_v4();
    let b = Uuid::new_v4();
    store.upsert(&a, "s", "v").await.unwrap();
    store.upsert(&b, "s", "v").await.unwrap();

    s.provision(&a, &secrets()).await.unwrap();
    s.provision(&b, &secrets()).await.unwrap();

    assert_ne!(s.project_dir(&a), s.project_dir(&b));
    assert_ne!(s.run_dir(&a), s.run_dir(&b));
    let (ua, ub) = (
        store.get(&a).await.unwrap().unwrap().uid_base,
        store.get(&b).await.unwrap().unwrap().uid_base,
    );
    assert!(
        ua.is_some() && ub.is_some(),
        "provision must allocate a uid"
    );
    assert_ne!(ua, ub, "two projects were given the same uid");
}

#[tokio::test]
async fn status_is_stopped_before_anything_is_started() {
    let (s, _store, _d, _r) = sandbox();
    assert_eq!(s.status(&Uuid::new_v4()).await.unwrap(), Status::Stopped);
}

#[tokio::test]
async fn stop_and_destroy_converge_on_a_project_that_was_never_started() {
    // Both have to be safe to call on nothing: the API calls stop before delete, and delete has to
    // succeed even when the sandbox is already gone or never existed.
    let (s, _store, _d, _r) = sandbox();
    let id = Uuid::new_v4();
    s.stop(&id).await.expect("stop on a stopped project");
    s.destroy(&id).await.expect("destroy on an absent project");
    s.destroy(&id).await.expect("destroy must be idempotent");
}

#[tokio::test]
async fn destroy_removes_the_project_tree() {
    require_root!();
    let (s, store, _d, _r) = sandbox();
    let id = Uuid::new_v4();
    store.upsert(&id, "s", "v").await.unwrap();
    s.provision(&id, &secrets()).await.unwrap();
    assert!(s.project_dir(&id).is_dir());

    s.destroy(&id).await.unwrap();
    assert!(
        !s.project_dir(&id).exists(),
        "project data outlived the project"
    );
    assert!(
        !s.run_dir(&id).exists(),
        "socket directory outlived the project"
    );
}

#[tokio::test]
async fn a_destroyed_project_keeps_its_uid_reserved() {
    require_root!();
    // The uid must not return to the pool: a later project receiving it would inherit ownership of
    // anything the old one left behind on disk.
    let (s, store, _d, _r) = sandbox();
    let id = Uuid::new_v4();
    store.upsert(&id, "s", "v").await.unwrap();
    s.provision(&id, &secrets()).await.unwrap();
    let uid = store.get(&id).await.unwrap().unwrap().uid_base.unwrap();

    s.destroy(&id).await.unwrap();
    store.delete(&id).await.unwrap();

    let next = Uuid::new_v4();
    store.upsert(&next, "s", "v").await.unwrap();
    s.provision(&next, &secrets()).await.unwrap();
    let next_uid = store.get(&next).await.unwrap().unwrap().uid_base.unwrap();
    assert_ne!(next_uid, uid, "a deleted project's uid was recycled");
}

#[tokio::test]
async fn starting_without_an_engine_binary_fails_rather_than_reporting_running() {
    // There is no `wheel-engine` on PATH in the test environment. The interesting property is that
    // this surfaces as an error: a spawn failure reported as success would leave the API telling a
    // user their project is up.
    let (s, store, _d, _r) = sandbox();
    let id = Uuid::new_v4();
    store.upsert(&id, "s", "v").await.unwrap();

    let started = s.start(&id, &secrets()).await;
    assert!(
        started.is_err(),
        "start succeeded with no engine binary present"
    );
    assert_eq!(
        s.status(&id).await.unwrap(),
        Status::Stopped,
        "a project that failed to start must not read as running"
    );
}
