//! The rotation's two load-bearing properties.
//!
//! `docs/runbooks/rotate-engine-secret.md` step 2 writes a column that is AES-GCM sealed, so no SQL
//! can produce a valid value. Code that writes it during an incident has exactly two ways to make
//! things worse: writing when it said it would not, and writing something the API can never open
//! again. Both are asserted here, and both are proven by mutation rather than by reading.
//!
//! These call the function directly. The first version drove the `examples/` binary through
//! `std::process::Command` and built it first with a nested `cargo build` — which passed locally and
//! then failed under `cargo llvm-cov`, because invoking cargo inside a cargo-driven test run
//! contends for the same target directory. Before that it did NOT build the binary at all and
//! silently tested a stale one. Two failures from the same decision: testing a subprocess instead of
//! a function.

#![cfg(feature = "sqlite")]

use wheel_api::admin::{rotate_engine_secret, Rotation};
use wheel_api::crypto;
use wheel_api::db::Db;

async fn seeded(key: &[u8; 32]) -> (Db, uuid::Uuid, Vec<u8>) {
    let path = std::env::temp_dir().join(format!("wheel-rot-{}.db", uuid::Uuid::new_v4()));
    let url = format!("sqlite://{}", path.display());
    let db = Db::connect(&url).await.expect("connect and migrate");

    let id = uuid::Uuid::new_v4();
    let sealed = crypto::seal(key, &crypto::generate_secret()).expect("seal");
    let vault = crypto::seal(key, &crypto::generate_secret()).expect("seal");

    wheel_api::db_execute!(
        &db,
        "INSERT INTO projects (id, owner_id, name, capabilities, status) \
         VALUES ($1, $2, $3, $4, $5)",
        id,
        "user_rot",
        "rot",
        serde_json::json!({"http": false}),
        "stopped"
    )
    .expect("insert project");
    wheel_api::db_execute!(
        &db,
        "INSERT INTO project_secrets (project_id, engine_secret_enc, vault_key_enc) \
         VALUES ($1, $2, $3)",
        id,
        sealed.clone(),
        vault
    )
    .expect("insert secrets");

    (db, id, sealed)
}

async fn stored(db: &Db, id: uuid::Uuid) -> Vec<u8> {
    let row: (Vec<u8>,) = wheel_api::db_fetch_one!(
        db,
        "SELECT engine_secret_enc FROM project_secrets WHERE project_id = $1",
        id
    )
    .expect("read back");
    row.0
}

/// A dry run that writes is worse than no tool: the operator's next decision is made on a false
/// belief about what already happened.
#[tokio::test]
async fn a_dry_run_does_not_write() {
    let key = [9u8; 32];
    let (db, id, before) = seeded(&key).await;

    let outcome = rotate_engine_secret(&db, &key, id, false)
        .await
        .expect("dry run");
    assert_eq!(outcome, Rotation::DryRun);
    assert_eq!(
        stored(&db, id).await,
        before,
        "the dry run rewrote the sealed secret"
    );
}

/// The rotated value has to be one the API can still open, or the project cannot start again.
#[tokio::test]
async fn apply_rotates_to_a_value_the_api_can_still_open() {
    let key = [7u8; 32];
    let (db, id, before) = seeded(&key).await;

    let outcome = rotate_engine_secret(&db, &key, id, true)
        .await
        .expect("apply");
    assert_eq!(outcome, Rotation::Rotated);

    let after = stored(&db, id).await;
    assert_ne!(after, before, "apply did not change the sealed secret");
    let opened = crypto::open(&key, &after).expect("the rotated secret must decrypt");
    assert!(!opened.expose().is_empty(), "rotated to an empty secret");
}

/// The wrong master key must stop BEFORE the write, not overwrite a good row with a value nothing
/// can open. This is the mistake the runbook calls unrecoverable.
#[tokio::test]
async fn a_wrong_master_key_refuses_instead_of_destroying_the_row() {
    let key = [3u8; 32];
    let (db, id, before) = seeded(&key).await;

    let e = rotate_engine_secret(&db, &[4u8; 32], id, true)
        .await
        .expect_err("a wrong master key was accepted");
    assert!(
        format!("{e:#}").contains("refusing to overwrite"),
        "the refusal must say why: {e:#}"
    );
    assert_eq!(
        stored(&db, id).await,
        before,
        "the row was overwritten despite the wrong key"
    );
}

/// A project that is not there is a stop, not a silent no-op.
#[tokio::test]
async fn an_unknown_project_is_an_error_naming_it() {
    let key = [5u8; 32];
    let (db, _id, _) = seeded(&key).await;
    let missing = uuid::Uuid::new_v4();

    let e = rotate_engine_secret(&db, &key, missing, true)
        .await
        .expect_err("an unknown project must not succeed");
    assert!(format!("{e:#}").contains(&missing.to_string()), "{e:#}");
}
