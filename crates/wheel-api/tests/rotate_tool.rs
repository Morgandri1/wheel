//! The rotation tool's two load-bearing properties, exercised against the real binary.
//!
//! `docs/runbooks/rotate-engine-secret.md` step 2 exists because `engine_secret_enc` is AES-GCM
//! sealed and no SQL can produce a valid value. A tool that writes that column at 3am during an
//! incident has exactly two ways to make things worse: writing when it said it would not, and
//! writing something the API can never open again. Both are asserted here.

#![cfg(feature = "sqlite")]

use base64::Engine as _;
use wheel_api::crypto;
use wheel_api::db::Db;

/// Build the example, then return its path.
///
/// It BUILDS rather than merely locating, and that is not belt-and-braces. `cargo test --test
/// rotate_tool` does not rebuild examples, so an earlier version of this test located a stale
/// binary and passed against a deliberately broken tool — the mutation went undetected because the
/// subject under test was a file from a previous compile. A test that shells out to an artifact is
/// only ever testing whichever artifact happens to be on disk unless it puts one there itself.
fn tool() -> std::path::PathBuf {
    let status = std::process::Command::new(env!("CARGO"))
        .args([
            "build",
            "-p",
            "wheel-api",
            "--example",
            "rotate-engine-secret",
        ])
        .status()
        .expect("build the rotation tool");
    assert!(status.success(), "the rotation tool did not build");

    let mut p = std::env::current_exe().expect("test binary path");
    p.pop(); // deps/
    p.pop(); // debug/
    p.push("examples");
    p.push("rotate-engine-secret");
    assert!(
        p.exists(),
        "built the tool but cannot find it at {}",
        p.display()
    );
    p
}

async fn seeded(key: &[u8; 32]) -> (String, uuid::Uuid, Vec<u8>) {
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

    (url, id, sealed)
}

async fn stored(url: &str, id: uuid::Uuid) -> Vec<u8> {
    let db = Db::connect(url).await.expect("reopen");
    let row: (Vec<u8>,) = wheel_api::db_fetch_one!(
        &db,
        "SELECT engine_secret_enc FROM project_secrets WHERE project_id = $1",
        id
    )
    .expect("read back");
    row.0
}

fn run(url: &str, key_b64: &str, args: &[&str]) -> (bool, String) {
    let out = std::process::Command::new(tool())
        .args(args)
        .env("STORE", url)
        .env("API_MASTER_KEY", key_b64)
        .output()
        .expect("run the rotation tool");
    let mut s = String::from_utf8_lossy(&out.stdout).to_string();
    s.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.success(), s)
}

/// A dry run that writes is worse than no tool: the operator's next decision is made on a false
/// belief about what already happened.
#[tokio::test]
async fn a_dry_run_does_not_write() {
    let key = [9u8; 32];
    let key_b64 = base64::engine::general_purpose::STANDARD.encode(key);
    let (url, id, before) = seeded(&key).await;

    let (ok, out) = run(&url, &key_b64, &[&id.to_string()]);
    assert!(ok, "dry run failed: {out}");
    assert!(out.contains("DRY RUN"), "{out}");

    assert_eq!(
        stored(&url, id).await,
        before,
        "the dry run rewrote the sealed secret"
    );
}

/// The rotated value has to be one the API can still open, or the project cannot start again.
#[tokio::test]
async fn apply_rotates_to_a_value_the_api_can_still_open() {
    let key = [7u8; 32];
    let key_b64 = base64::engine::general_purpose::STANDARD.encode(key);
    let (url, id, before) = seeded(&key).await;

    let (ok, out) = run(&url, &key_b64, &[&id.to_string(), "--apply"]);
    assert!(ok, "apply failed: {out}");

    let after = stored(&url, id).await;
    assert_ne!(after, before, "--apply did not change the sealed secret");

    let opened = crypto::open(&key, &after).expect("the rotated secret must decrypt");
    assert!(!opened.expose().is_empty(), "rotated to an empty secret");

    // Neither secret may reach a terminal, a log, or a shoulder.
    let old = crypto::open(&key, &before).expect("seeded secret decrypts");
    assert!(
        !out.contains(opened.expose()),
        "printed the new secret: {out}"
    );
    assert!(!out.contains(old.expose()), "printed the old secret: {out}");
}

/// The wrong master key must stop BEFORE the write, not overwrite a good row with a value nothing
/// can open. This is the mistake the runbook calls unrecoverable.
#[tokio::test]
async fn a_wrong_master_key_refuses_instead_of_destroying_the_row() {
    let key = [3u8; 32];
    // The correct key seeds the row; the tool is then handed a different one.
    let (url, id, before) = seeded(&key).await;

    let wrong = base64::engine::general_purpose::STANDARD.encode([4u8; 32]);
    let (ok, out) = run(&url, &wrong, &[&id.to_string(), "--apply"]);
    assert!(!ok, "a wrong master key was accepted: {out}");
    assert!(out.contains("refusing to overwrite"), "{out}");

    assert_eq!(
        stored(&url, id).await,
        before,
        "the row was overwritten despite the wrong key"
    );
}
