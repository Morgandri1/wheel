// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

//! Operator actions that are not routes.
//!
//! `rotate_engine_secret` lives here rather than inside the `examples/` binary so it can be tested
//! directly. The first version put the logic in the example and drove it from a test that shelled
//! out to `cargo build` — which worked locally and then failed under `cargo llvm-cov`, because
//! invoking cargo from inside a cargo-driven test run contends for the same target directory. A
//! function is testable without any of that.

use crate::crypto::{self, Secret};
use crate::db::Db;
use anyhow::{bail, Context, Result};
use uuid::Uuid;

/// What a rotation did, so the caller can report it without the tool re-deriving it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rotation {
    /// `apply` was false: the row was read and verified, and nothing was written.
    DryRun,
    Rotated,
}

/// Replace one project's sealed engine secret.
///
/// The current value is decrypted FIRST, and nothing is written if that fails. A master key that
/// cannot open the existing row is the wrong key for this database, and using it would replace a
/// good row with one the API can never decrypt — the unrecoverable mistake in the runbook.
///
/// The new secret is never returned or logged: nothing human needs it, and the engine receives it
/// from the API on its next start.
pub async fn rotate_engine_secret(
    db: &Db,
    master_key: &[u8; 32],
    id: Uuid,
    apply: bool,
) -> Result<Rotation> {
    let existing: Option<(Vec<u8>,)> = crate::db_fetch_optional!(
        db,
        "SELECT engine_secret_enc FROM project_secrets WHERE project_id = $1",
        id
    )?;
    let Some((sealed,)) = existing else {
        bail!(
            "no project_secrets row for {id} — is that the right project, and the right database?"
        );
    };
    crypto::open(master_key, &sealed).context(
        "this API_MASTER_KEY cannot decrypt the existing secret; refusing to overwrite it",
    )?;

    if !apply {
        return Ok(Rotation::DryRun);
    }

    let fresh: Secret = crypto::generate_secret();
    let resealed = crypto::seal(master_key, &fresh).context("sealing the new secret")?;
    crate::db_execute!(
        db,
        "UPDATE project_secrets SET engine_secret_enc = $1 WHERE project_id = $2",
        resealed,
        id
    )?;
    Ok(Rotation::Rotated)
}
