// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

//! API tokens managed against the local store: the first-boot operator token, and `wheeld token`.
//!
//! Whoever can read the data directory can already read `master.key`, which is strictly more power
//! than any token, so the directory is the boundary here and no daemon has to be running. The token
//! itself is printed on stdout and nowhere else, so `$(wheeld token create)` captures exactly it;
//! everything else goes to stderr.

use anyhow::{bail, Context, Result};
use std::io::Write;
use std::path::{Path, PathBuf};
use wheel_api::auth::{api_token, local};
use wheel_api::db::Db;

/// The token-only account `wheeld` creates on an empty store. `.invalid` is reserved (RFC 2606),
/// so it is nobody's real address.
pub const OWNER_EMAIL: &str = "operator@wheeld.invalid";
pub const OPERATOR_TOKEN_FILE: &str = "operator-token";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenCommand {
    Create { name: String, email: Option<String> },
    List,
    Revoke { id: String },
}

/// First start against an empty store: the token-only owner, and its token in
/// `<data-dir>/operator-token`. `None` when the store already had accounts.
pub async fn bootstrap_operator(db: &Db, data_dir: &Path) -> Result<Option<PathBuf>> {
    let Some(issued) = api_token::bootstrap_owner(db, OWNER_EMAIL, "operator").await? else {
        return Ok(None);
    };
    let path = data_dir.join(OPERATOR_TOKEN_FILE);
    crate::supervise::write_new_private(&path, &format!("{}\n", issued.token))
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(Some(path))
}

pub async fn run(
    cmd: TokenCommand,
    data_dir: &Path,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<()> {
    let db = open_store(data_dir).await?;
    match cmd {
        TokenCommand::Create { name, email } => {
            let owner = match &email {
                Some(email) => local::find_user_by_email(&db, email)
                    .await?
                    .with_context(|| format!("no account {email:?} in this store"))?,
                None => local::find_token_only_user(&db, OWNER_EMAIL)
                    .await?
                    .context(
                        "this store has no token-only owner: it already had accounts when \
                         wheeld first started. Pass --email <account>",
                    )?,
            };
            let issued = api_token::issue(&db, &owner.id.to_string(), &name, None).await?;
            writeln!(out, "{}", issued.token)?;
            writeln!(
                err,
                "token {} ({}) for {}",
                issued.id, issued.name, owner.email
            )?;
        }
        TokenCommand::List => {
            let tokens = api_token::list_all(&db).await?;
            if tokens.is_empty() {
                writeln!(err, "no tokens")?;
            }
            if !tokens.is_empty() {
                writeln!(
                    out,
                    "{:<36}  {:<28}  {:<16}  {:<19}  {:<19}  REVOKED",
                    "ID", "ACCOUNT", "NAME", "CREATED", "LAST USED"
                )?;
            }
            for t in tokens {
                let when = |at: Option<String>| at.unwrap_or_else(|| "-".into());
                writeln!(
                    out,
                    "{:<36}  {:<28}  {:<16}  {:<19}  {:<19}  {}",
                    t.id,
                    t.email.as_deref().unwrap_or(&t.user_id),
                    t.name,
                    t.created_at.format("%Y-%m-%d %H:%M:%S").to_string(),
                    when(
                        t.last_used_at
                            .map(|d| d.format("%Y-%m-%d %H:%M:%S").to_string())
                    ),
                    when(
                        t.revoked_at
                            .map(|d| d.format("%Y-%m-%d %H:%M:%S").to_string())
                    ),
                )?;
            }
        }
        TokenCommand::Revoke { id } => {
            let parsed = uuid::Uuid::parse_str(id.trim())
                .with_context(|| format!("{id:?} is not a token id"))?;
            if !api_token::revoke(&db, &parsed, None).await? {
                bail!("no token {parsed} in this store");
            }
            writeln!(err, "revoked {parsed} and every token it minted")?;
        }
    }
    Ok(())
}

/// The store `wheeld` itself serves from. Refuses to invent one: a mistyped `--data-dir` should
/// say so, not create an empty install and report that it has no tokens.
async fn open_store(data_dir: &Path) -> Result<Db> {
    let url = crate::supervise::store_url(data_dir);
    if std::env::var_os("STORE").is_none() && !data_dir.join("wheel.db").exists() {
        bail!(
            "no wheeld store in {}: start wheeld once, or pass --data-dir",
            data_dir.display()
        );
    }
    Db::connect(&url).await.context("opening the store")
}
