// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

//! Long-lived API tokens: how an operator, a script or a desktop client authenticates without a
//! browser, against a local `wheeld` and the cloud API alike.
//!
//! A token is `wht_` followed by 32 random bytes in base64url. The store keeps only its SHA-256, so
//! a copy of the database is not a copy of anyone's credentials. A fast hash is the right one here:
//! the token has 256 bits of entropy, and argon2 exists for secrets a person chose.
//!
//! The digest is also the lookup key. An index probe can then only take time that depends on a
//! value nobody can steer towards a stored one, which is what makes looking it up by equality safe.
//!
//! A token may mint tokens, so revocation follows lineage: revoking one revokes everything it
//! minted, transitively. Whoever used a leaked token to mint successors loses them with it.

use crate::db::Db;
use crate::error::{ApiError, ApiResult};
use base64::Engine as _;
use chrono::{DateTime, Utc};
use serde::Serialize;
use sha2::Digest as _;
use uuid::Uuid;

pub const PREFIX: &str = "wht_";
pub const MAX_NAME_LEN: usize = 64;

pub fn is_api_token(token: &str) -> bool {
    token.starts_with(PREFIX)
}

fn digest(token: &str) -> String {
    hex::encode(sha2::Sha256::digest(token.as_bytes()))
}

fn generate() -> String {
    use rand::RngCore;
    let mut secret = [0u8; 32];
    rand::rng().fill_bytes(&mut secret);
    format!(
        "{PREFIX}{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(secret)
    )
}

/// A token as it is issued: the only moment anything holds its value. Deliberately not `Debug`.
pub struct Issued {
    pub id: Uuid,
    pub name: String,
    pub token: String,
    pub created_at: DateTime<Utc>,
}

/// Who a live token speaks for, and which token it was.
pub struct Verified {
    pub user_id: String,
    pub token_id: Uuid,
}

/// What an account may see of its own tokens: never the token, never its hash.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct TokenInfo {
    pub id: Uuid,
    pub name: String,
    pub minted_by: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
}

/// A token and the account it belongs to, for the operator's view of the whole store. `email` is
/// absent for a subject with no local account (an identity provider's user).
#[derive(Debug, Clone, Serialize)]
pub struct AccountToken {
    pub id: Uuid,
    pub user_id: String,
    pub email: Option<String>,
    pub name: String,
    pub minted_by: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
}

#[derive(sqlx::FromRow)]
struct TokenRow {
    id: Uuid,
    user_id: String,
    name: String,
    minted_by: Option<Uuid>,
    created_at: DateTime<Utc>,
    last_used_at: Option<DateTime<Utc>>,
    revoked_at: Option<DateTime<Utc>>,
}

pub fn validate_name(raw: &str) -> Result<String, String> {
    let name = raw.trim();
    if name.is_empty() {
        return Err("token name must not be empty".into());
    }
    if name.chars().count() > MAX_NAME_LEN {
        return Err(format!(
            "token name must be at most {MAX_NAME_LEN} characters"
        ));
    }
    if name.chars().any(char::is_control) {
        return Err("token name must not contain control characters".into());
    }
    Ok(name.to_string())
}

pub async fn issue(
    db: &Db,
    user_id: &str,
    name: &str,
    minted_by: Option<Uuid>,
) -> ApiResult<Issued> {
    let name = validate_name(name).map_err(ApiError::BadRequest)?;
    let id = Uuid::new_v4();
    let token = generate();
    let (created_at,): (DateTime<Utc>,) = crate::db_fetch_one!(
        db,
        "INSERT INTO api_tokens (id, user_id, name, token_hash, minted_by) \
         VALUES ($1, $2, $3, $4, $5) RETURNING created_at",
        id,
        user_id,
        &name,
        digest(&token),
        minted_by
    )?;
    Ok(Issued {
        id,
        name,
        token,
        created_at,
    })
}

/// The subject a live token belongs to, recording that it was used.
///
/// Unknown and revoked are one answer, so a caller learns nothing about which tokens exist. The
/// revocation check and the use stamp are one statement, so a token revoked mid-request cannot be
/// stamped as used after it died.
pub async fn verify(db: &Db, token: &str) -> Result<Verified, ApiError> {
    const PG: &str = "UPDATE api_tokens SET last_used_at = now() \
         WHERE token_hash = $1 AND revoked_at IS NULL RETURNING user_id, id";
    const SQLITE: &str = "UPDATE api_tokens \
         SET last_used_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') \
         WHERE token_hash = $1 AND revoked_at IS NULL RETURNING user_id, id";
    let row: Option<(String, Uuid)> =
        crate::db_fetch_optional!(db, db.pick(PG, SQLITE), digest(token))?;
    row.map(|(user_id, token_id)| Verified { user_id, token_id })
        .ok_or(ApiError::Unauthorized("api token is unknown or revoked"))
}

pub async fn list_for_user(db: &Db, user_id: &str) -> ApiResult<Vec<TokenInfo>> {
    Ok(crate::db_fetch_all!(
        db,
        "SELECT id, name, minted_by, created_at, last_used_at, revoked_at FROM api_tokens \
         WHERE user_id = $1 ORDER BY created_at DESC, id",
        user_id
    )?)
}

/// Every token in the store, with the local account each belongs to.
///
/// The account is looked up per subject rather than joined: `users.id` is a native uuid, while the
/// subject is text that may not be a uuid at all (an identity provider's `sub`), and the two stores
/// disagree on how a uuid compares to text.
pub async fn list_all(db: &Db) -> ApiResult<Vec<AccountToken>> {
    let rows: Vec<TokenRow> = crate::db_fetch_all!(
        db,
        "SELECT id, user_id, name, minted_by, created_at, last_used_at, revoked_at \
         FROM api_tokens ORDER BY created_at, id"
    )?;
    let mut emails = std::collections::HashMap::<String, Option<String>>::new();
    let mut listed = Vec::with_capacity(rows.len());
    for r in rows {
        if !emails.contains_key(&r.user_id) {
            let email = match Uuid::parse_str(&r.user_id) {
                Ok(id) => crate::auth::local::find_user(db, &id)
                    .await?
                    .map(|u| u.email),
                Err(_) => None,
            };
            emails.insert(r.user_id.clone(), email);
        }
        listed.push(AccountToken {
            email: emails[&r.user_id].clone(),
            id: r.id,
            user_id: r.user_id,
            name: r.name,
            minted_by: r.minted_by,
            created_at: r.created_at,
            last_used_at: r.last_used_at,
            revoked_at: r.revoked_at,
        });
    }
    Ok(listed)
}

/// Revoke a token and every token it minted, transitively, confined to `owner`'s tokens when one
/// is given. Revoking twice keeps the first time. Returns whether there was such a token.
pub async fn revoke(db: &Db, id: &Uuid, owner: Option<&str>) -> ApiResult<bool> {
    let now = db.pick("now()", "strftime('%Y-%m-%dT%H:%M:%fZ', 'now')");
    let scope = if owner.is_some() {
        " AND user_id = $2"
    } else {
        ""
    };
    let sql = format!(
        "WITH RECURSIVE family(id) AS ( \
             SELECT id FROM api_tokens WHERE id = $1{scope} \
             UNION SELECT t.id FROM api_tokens t JOIN family f ON t.minted_by = f.id \
         ) \
         UPDATE api_tokens SET revoked_at = COALESCE(revoked_at, {now}) \
         WHERE id IN (SELECT id FROM family)"
    );
    let changed = match owner {
        Some(owner) => crate::db_execute!(db, &sql, id, owner)?,
        None => crate::db_execute!(db, &sql, id)?,
    };
    Ok(changed > 0)
}

/// First boot of a store with no accounts: a token-only owner and its first token.
///
/// `None` once any account exists. An install that already has users says which of them a token
/// belongs to, rather than having a new, empty account invented for it.
pub async fn bootstrap_owner(db: &Db, email: &str, token_name: &str) -> ApiResult<Option<Issued>> {
    if crate::auth::local::count_users(db).await? > 0 {
        return Ok(None);
    }
    let owner = crate::auth::local::create_token_only_user(db, email).await?;
    issue(db, &owner.id.to_string(), token_name, None)
        .await
        .map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_token_is_the_prefix_and_256_bits() {
        let t = generate();
        assert!(is_api_token(&t), "{t}");
        let secret = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(&t[PREFIX.len()..])
            .unwrap();
        assert_eq!(secret.len(), 32);
        assert_ne!(generate(), t, "two tokens were identical");
    }

    #[test]
    fn the_digest_is_sha256_hex_and_not_the_token() {
        let d = digest("wht_example");
        assert_eq!(d.len(), 64);
        assert!(d.chars().all(|c| c.is_ascii_hexdigit()));
        assert!(!d.contains("wht_"));
    }

    #[test]
    fn names_are_trimmed_bounded_and_printable() {
        assert_eq!(validate_name("  ci  ").unwrap(), "ci");
        assert!(validate_name("   ").is_err());
        assert!(validate_name(&"n".repeat(MAX_NAME_LEN)).is_ok());
        assert!(validate_name(&"n".repeat(MAX_NAME_LEN + 1)).is_err());
        assert!(validate_name("a\u{7}b").is_err());
    }
}
