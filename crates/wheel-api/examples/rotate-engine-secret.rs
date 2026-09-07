//! Rotate one project's `WHEEL_ENGINE_SECRET`. **Dry run unless `--apply`.**
//!
//! `docs/runbooks/rotate-engine-secret.md` documents the procedure and, until this existed, had a
//! step nobody could perform: `project_secrets.engine_secret_enc` is AES-GCM sealed with
//! `API_MASTER_KEY`, so no SQL can produce a valid value, and nothing but project create had ever
//! written that table.
//!
//!     DATABASE_URL=… API_MASTER_KEY=… cargo run -p wheel-api --example rotate-engine-secret -- <project-uuid>
//!     …                                                                          -- <project-uuid> --apply
//!
//! Then restart the project. `start` and `restart` both call `reprovision` first, so one restart
//! re-sends the new secret to the host AND respawns the engine with it — there is no window where
//! the host presents a new bearer to an engine still running the old one.
//!
//! WHAT IT WILL NOT DO:
//! - print a secret, old or new, in either mode;
//! - touch `vault_key_enc` — rotating that is a re-encryption migration, and the credentials it
//!   protects have to be regenerated at their providers anyway (043);
//! - restart anything. Rotating and restarting are separate so the operator chooses the moment the
//!   project goes down.
//!
//! It is an `examples/` target: not in any shipped binary and unreachable from the library.
//!
//! ROTATE ONLY AFTER THE CARRIER IS CLOSED (043: after #17 scrubs the secret from the engine's
//! environ). Rotating into a still-open exposure re-exposes the new secret immediately.

use anyhow::{anyhow, bail, Context, Result};
use base64::Engine as _;
use wheel_api::crypto;
use wheel_api::db::Db;

fn master_key() -> Result<[u8; 32]> {
    let raw = std::env::var("API_MASTER_KEY").context("API_MASTER_KEY is not set")?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(raw.trim())
        .context("API_MASTER_KEY must be valid base64")?;
    let len = bytes.len();
    <[u8; 32]>::try_from(bytes.as_slice())
        .map_err(|_| anyhow!("API_MASTER_KEY must decode to exactly 32 bytes, got {len}"))
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let apply = args.iter().any(|a| a == "--apply");
    let id = args
        .iter()
        .find(|a| !a.starts_with("--"))
        .ok_or_else(|| anyhow!("usage: rotate-engine-secret <project-uuid> [--apply]"))?;
    let id: uuid::Uuid = id.parse().context("the project id must be a uuid")?;

    let url = std::env::var("STORE")
        .or_else(|_| std::env::var("DATABASE_URL"))
        .context("set STORE or DATABASE_URL")?;
    let key = master_key()?;
    let db = Db::connect(&url).await.context("connecting to the store")?;

    // Read the CURRENT value first and decrypt it. Nothing is written until this succeeds: if the
    // master key in this environment is not the one the row was sealed with, the honest outcome is
    // to stop here, not to overwrite a good row with a value the API will never be able to open.
    let existing: Option<(Vec<u8>,)> = wheel_api::db_fetch_optional!(
        &db,
        "SELECT engine_secret_enc FROM project_secrets WHERE project_id = $1",
        id
    )?;
    let Some((sealed,)) = existing else {
        bail!(
            "no project_secrets row for {id} — is that the right project, and the right database?"
        );
    };
    crypto::open(&key, &sealed).context(
        "this API_MASTER_KEY cannot decrypt the existing secret; refusing to overwrite it",
    )?;

    println!("project      {id}");
    println!("store        {}", scheme_of(&url));
    println!("current row  present, and decrypts with this API_MASTER_KEY");

    if !apply {
        println!();
        println!("DRY RUN — nothing written. Re-run with --apply to rotate.");
        println!(
            "After applying, restart the project so the host and the engine take the new value:"
        );
        println!("  POST /v1/projects/{id}/restart");
        return Ok(());
    }

    let fresh = crypto::generate_secret();
    let resealed = crypto::seal(&key, &fresh).context("sealing the new secret")?;
    let n = wheel_api::db_execute!(
        &db,
        "UPDATE project_secrets SET engine_secret_enc = $1 WHERE project_id = $2",
        resealed,
        id
    )?;
    if n != 1 {
        bail!("expected to update exactly one row, updated {n} — investigate before restarting");
    }

    println!();
    println!("ROTATED. The new secret is not printed and is not needed by a human.");
    println!("The engine is still running with the OLD value until you restart it:");
    println!("  POST /v1/projects/{id}/restart");
    println!("Then confirm the board answers 200 — a 502 means host and engine disagree.");
    Ok(())
}

/// The scheme only, so a connection string with a password never reaches the terminal.
fn scheme_of(url: &str) -> &str {
    match url.split_once("://") {
        Some((s, _)) => s,
        None => "unknown",
    }
}
