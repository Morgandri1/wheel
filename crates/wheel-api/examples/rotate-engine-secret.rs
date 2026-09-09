// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

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

use anyhow::{anyhow, Context, Result};
use base64::Engine as _;
use wheel_api::admin::{rotate_engine_secret, Rotation};
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

/// The scheme only, so a connection string with a password never reaches the terminal.
fn scheme_of(url: &str) -> &str {
    match url.split_once("://") {
        Some((s, _)) => s,
        None => "unknown",
    }
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

    println!("project      {id}");
    println!("store        {}", scheme_of(&url));

    match rotate_engine_secret(&db, &key, id, apply).await? {
        Rotation::DryRun => {
            println!("current row  present, and decrypts with this API_MASTER_KEY");
            println!();
            println!("DRY RUN — nothing written. Re-run with --apply to rotate.");
        }
        Rotation::Rotated => {
            println!();
            println!("ROTATED. The new secret is not printed and is not needed by a human.");
        }
    }
    println!("The engine keeps the OLD value until the project is restarted:");
    println!("  POST /v1/projects/{id}/restart");
    println!("Then confirm the board answers 200 — a 502 means host and engine disagree.");
    Ok(())
}
