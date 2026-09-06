//! Local identity: users, passwords, and revocable sessions.
//!
//! This is the built-in `AUTH_MODE=local` provider. It exists so the product works without a
//! third-party identity service, and it is deliberately shaped so swapping to one (`AUTH_MODE=jwks`)
//! changes configuration rather than code: both paths end at the same `VerifiedUser`, and the
//! ownership extractor never learns which one produced it.

use crate::db::Db;
use crate::error::{ApiError, ApiResult};
use argon2::password_hash::{
    rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString,
};
use argon2::Argon2;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Minimum password length.
///
/// Length is the only property worth enforcing here. Composition rules (a digit, a symbol) push
/// people toward `Password1!` and measurably weaken what they choose, so we ask for length and
/// leave the rest alone.
pub const MIN_PASSWORD_LEN: usize = 10;

/// Long enough that a paste of a password manager entry is never rejected, short enough that
/// hashing cannot be turned into a CPU-exhaustion vector.
pub const MAX_PASSWORD_LEN: usize = 1024;

pub const SESSION_TTL_DAYS: i64 = 7;

#[derive(Debug, Clone, Serialize)]
pub struct User {
    pub id: Uuid,
    pub email: String,
    pub created_at: DateTime<Utc>,
}

#[derive(sqlx::FromRow)]
struct UserRow {
    id: Uuid,
    email: String,
    password_hash: String,
    created_at: DateTime<Utc>,
}

/// Claims in a locally issued session token.
#[derive(Debug, Serialize, Deserialize)]
pub struct SessionClaims {
    /// User id — the same `sub` the rest of the system keys ownership on.
    pub sub: String,
    pub iss: String,
    pub exp: i64,
    pub nbf: i64,
    /// Session id, so a token can be revoked without waiting for it to expire.
    pub sid: String,
}

pub fn validate_email(raw: &str) -> Result<String, String> {
    let email = raw.trim();
    if email.is_empty() {
        return Err("email must not be empty".into());
    }
    if email.chars().count() > 320 {
        return Err("email is too long".into());
    }
    // Deliberately minimal. Full RFC 5322 validation rejects addresses that genuinely deliver, and
    // the only real proof an address works is sending to it.
    let Some((local, domain)) = email.split_once('@') else {
        return Err("email must contain @".into());
    };
    if local.is_empty()
        || domain.is_empty()
        || !domain.contains('.')
        || domain.starts_with('.')
        || domain.ends_with('.')
    {
        return Err("email is not a valid address".into());
    }
    if email.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("email must not contain whitespace or control characters".into());
    }
    Ok(email.to_ascii_lowercase())
}

pub fn validate_password(password: &str) -> Result<(), String> {
    // Count characters, not bytes: a 10-character passphrase in a non-Latin script is not short.
    let len = password.chars().count();
    if len < MIN_PASSWORD_LEN {
        return Err(format!(
            "password must be at least {MIN_PASSWORD_LEN} characters"
        ));
    }
    if len > MAX_PASSWORD_LEN {
        return Err(format!(
            "password must be at most {MAX_PASSWORD_LEN} characters"
        ));
    }
    Ok(())
}

pub fn hash_password(password: &str) -> ApiResult<String> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("hashing password: {e}")))
}

fn verify_password(password: &str, encoded: &str) -> bool {
    let Ok(parsed) = PasswordHash::new(encoded) else {
        // A row we cannot parse is a corrupt hash, not a match.
        return false;
    };
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok()
}

/// A hash to verify against when the account does not exist.
///
/// Skipping the hash for an unknown email makes login measurably faster for addresses that are not
/// registered, which turns the endpoint into an account-existence oracle. Verifying against a real
/// argon2 hash costs the same time as the genuine path and always fails.
fn dummy_hash() -> &'static str {
    // Generated once from a random password; its plaintext is unknown and irrelevant.
    "$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHRzYWx0c2FsdA$5s4/dGvBGnJvVOtY0Xh3TfCJyaS3rSU0aVEEeC0dxKQ"
}

pub async fn create_user(db: &Db, email: &str, password: &str) -> ApiResult<User> {
    let email = validate_email(email).map_err(ApiError::BadRequest)?;
    validate_password(password).map_err(ApiError::BadRequest)?;
    let hash = hash_password(password)?;
    let id = Uuid::new_v4();

    let row: Result<UserRow, _> = crate::db_fetch_one!(
        db,
        "INSERT INTO users (id, email, password_hash) VALUES ($1, $2, $3) \
         RETURNING id, email, password_hash, created_at",
        id,
        &email,
        &hash
    );
    let row = row.map_err(|e| match &e {
        // A conflict the caller caused rather than a 500 — but see the note in the signup handler
        // about what we actually tell them.
        _ if crate::db::is_unique_violation(&e) => {
            ApiError::Conflict("an account with that email already exists".into())
        }
        _ => ApiError::from(e),
    })?;

    Ok(User {
        id: row.id,
        email: row.email,
        created_at: row.created_at,
    })
}

/// Verify an email/password pair.
///
/// Returns `None` for every failure — unknown email, wrong password, malformed input — and takes
/// the same work in each case, so the caller cannot distinguish them and neither can an attacker.
pub async fn authenticate(db: &Db, email: &str, password: &str) -> Option<User> {
    let email = validate_email(email).ok();
    let row: Option<UserRow> = match &email {
        Some(e) => crate::db_fetch_optional!(
            db,
            "SELECT id, email, password_hash, created_at FROM users WHERE email = $1",
            e
        )
        .ok()
        .flatten(),
        None => None,
    };

    match row {
        Some(user) if verify_password(password, &user.password_hash) => Some(User {
            id: user.id,
            email: user.email,
            created_at: user.created_at,
        }),
        Some(_) => None,
        None => {
            // Burn the same argon2 work we would have spent on a real account.
            let _ = verify_password(password, dummy_hash());
            None
        }
    }
}

pub async fn find_user(db: &Db, id: &Uuid) -> ApiResult<Option<User>> {
    let row: Option<UserRow> = crate::db_fetch_optional!(
        db,
        "SELECT id, email, password_hash, created_at FROM users WHERE id = $1",
        id
    )?;
    Ok(row.map(|r| User {
        id: r.id,
        email: r.email,
        created_at: r.created_at,
    }))
}

pub async fn change_password(db: &Db, user_id: &Uuid, current: &str, new: &str) -> ApiResult<()> {
    validate_password(new).map_err(ApiError::BadRequest)?;

    let row: UserRow = crate::db_fetch_one!(
        db,
        "SELECT id, email, password_hash, created_at FROM users WHERE id = $1",
        user_id
    )?;

    // Proving the current password is what stops a stolen session token from becoming a permanent
    // takeover: an attacker with the token still cannot lock the owner out.
    if !verify_password(current, &row.password_hash) {
        return Err(ApiError::Unauthorized("current password did not match"));
    }

    let hash = hash_password(new)?;

    // Both statements or neither. Every other session dies with the old password — if it was
    // changed because it was compromised, leaving the old sessions alive defeats the point — and a
    // new password whose revocation did not commit would be exactly that failure, silently.
    //
    // Written out per backend rather than through the dispatch macros because a transaction is a
    // connection, not a pool, and sqlx types it per database.
    const SET_PASSWORD: &str = "UPDATE users SET password_hash = $2 WHERE id = $1";
    const REVOKE_SESSIONS: &str = "DELETE FROM sessions WHERE user_id = $1";
    match db {
        #[cfg(feature = "postgres")]
        Db::Pg(pool) => {
            let mut tx = pool.begin().await?;
            sqlx::query(SET_PASSWORD)
                .bind(user_id)
                .bind(&hash)
                .execute(&mut *tx)
                .await?;
            sqlx::query(REVOKE_SESSIONS)
                .bind(user_id)
                .execute(&mut *tx)
                .await?;
            tx.commit().await?;
        }
        #[cfg(feature = "sqlite")]
        Db::Sqlite(pool) => {
            let mut tx = pool.begin().await?;
            sqlx::query(SET_PASSWORD)
                .bind(user_id)
                .bind(&hash)
                .execute(&mut *tx)
                .await?;
            sqlx::query(REVOKE_SESSIONS)
                .bind(user_id)
                .execute(&mut *tx)
                .await?;
            tx.commit().await?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------- sessions

pub struct IssuedSession {
    pub token: String,
    pub expires_at: DateTime<Utc>,
}

pub async fn issue_session(
    db: &Db,
    user_id: &Uuid,
    secret: &str,
    issuer: &str,
) -> ApiResult<IssuedSession> {
    let sid = Uuid::new_v4();
    let now = Utc::now();
    let expires_at = now + Duration::days(SESSION_TTL_DAYS);

    crate::db_execute!(
        db,
        "INSERT INTO sessions (id, user_id, expires_at) VALUES ($1, $2, $3)",
        sid,
        user_id,
        expires_at
    )?;

    let claims = SessionClaims {
        sub: user_id.to_string(),
        iss: issuer.to_string(),
        exp: expires_at.timestamp(),
        nbf: now.timestamp() - 60,
        sid: sid.to_string(),
    };
    let token = jsonwebtoken::encode(
        &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::HS256),
        &claims,
        &jsonwebtoken::EncodingKey::from_secret(secret.as_bytes()),
    )
    .map_err(|e| ApiError::Internal(anyhow::anyhow!("signing session: {e}")))?;

    Ok(IssuedSession { token, expires_at })
}

/// Verify a locally issued session token.
///
/// Signature and claims are checked first, then the session row: a stateless JWT alone cannot be
/// logged out, and "log out" that leaves the token working is not a logout.
pub async fn verify_session(
    db: &Db,
    token: &str,
    secret: &str,
    issuer: &str,
) -> Result<String, ApiError> {
    let mut v = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::HS256);
    v.set_issuer(&[issuer]);
    v.validate_exp = true;
    v.validate_nbf = true;
    v.validate_aud = false;
    v.leeway = 5;

    let data = jsonwebtoken::decode::<SessionClaims>(
        token,
        &jsonwebtoken::DecodingKey::from_secret(secret.as_bytes()),
        &v,
    )
    .map_err(|_| ApiError::Unauthorized("invalid session token"))?;

    let sid = Uuid::parse_str(&data.claims.sid)
        .map_err(|_| ApiError::Unauthorized("session id is not a uuid"))?;

    // Expiry is judged by the database clock on both backends, so replicas cannot disagree about
    // whether a session is still alive.
    const PG: &str = "SELECT user_id FROM sessions WHERE id = $1 AND expires_at > now()";
    const SQLITE: &str = "SELECT user_id FROM sessions \
         WHERE id = $1 AND expires_at > strftime('%Y-%m-%dT%H:%M:%fZ', 'now')";
    let live: Option<(Uuid,)> = crate::db_fetch_optional!(db, db.pick(PG, SQLITE), sid)?;
    let live = live.map(|r| r.0);

    match live {
        Some(user_id) if user_id.to_string() == data.claims.sub => Ok(data.claims.sub),
        // A valid signature over a revoked session, or one whose subject was tampered with.
        _ => Err(ApiError::Unauthorized("session is no longer valid")),
    }
}

pub async fn revoke_session(db: &Db, token: &str, secret: &str, issuer: &str) -> ApiResult<()> {
    let mut v = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::HS256);
    v.set_issuer(&[issuer]);
    // Accept an expired token here: logging out of an already-expired session should succeed
    // quietly rather than error, and revoking it is harmless.
    v.validate_exp = false;
    v.validate_nbf = false;
    v.validate_aud = false;

    if let Ok(data) = jsonwebtoken::decode::<SessionClaims>(
        token,
        &jsonwebtoken::DecodingKey::from_secret(secret.as_bytes()),
        &v,
    ) {
        if let Ok(sid) = Uuid::parse_str(&data.claims.sid) {
            crate::db_execute!(db, "DELETE FROM sessions WHERE id = $1", sid)?;
        }
    }
    Ok(())
}

/// Delete sessions that have already expired. Idempotent, safe in every replica.
pub async fn sweep(db: &Db) -> anyhow::Result<u64> {
    const PG: &str = "DELETE FROM sessions WHERE expires_at < now()";
    const SQLITE: &str =
        "DELETE FROM sessions WHERE expires_at < strftime('%Y-%m-%dT%H:%M:%fZ', 'now')";
    Ok(crate::db_execute!(db, db.pick(PG, SQLITE))?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emails_are_normalised_and_obvious_junk_is_refused() {
        // Surrounding whitespace is trimmed, not rejected: people paste addresses with it, and a
        // trailing newline is a paste artefact rather than a different address.
        assert_eq!(
            validate_email("  Alice@Example.COM ").unwrap(),
            "alice@example.com"
        );
        assert_eq!(validate_email("a@example.com\n").unwrap(), "a@example.com");

        for bad in [
            "",
            "   ",
            "no-at-sign",
            "@example.com",
            "a@",
            "a@nodot",
            "a@.example.com",
            "a@example.com.",
            // Whitespace *inside* the address is a different matter — it cannot be trimmed away
            // and is never a real address.
            "a b@example.com",
            "a@exa mple.com",
        ] {
            assert!(validate_email(bad).is_err(), "accepted {bad:?}");
        }
    }

    #[test]
    fn password_length_is_measured_in_characters() {
        assert!(validate_password("short").is_err());
        assert!(validate_password("0123456789").is_ok());
        // Ten characters, thirty bytes: a byte-length check would wrongly accept fewer.
        assert!(validate_password(&"é".repeat(10)).is_ok());
        assert!(validate_password(&"a".repeat(9)).is_err());
        assert!(validate_password(&"a".repeat(MAX_PASSWORD_LEN + 1)).is_err());
    }

    #[test]
    fn hashes_are_salted_and_verify() {
        let a = hash_password("correct horse battery").unwrap();
        let b = hash_password("correct horse battery").unwrap();
        assert_ne!(
            a, b,
            "identical passwords produced identical hashes: salt is not being used"
        );
        assert!(verify_password("correct horse battery", &a));
        assert!(!verify_password("wrong horse battery", &a));
        assert!(a.starts_with("$argon2id$"), "expected argon2id, got {a}");
    }

    #[test]
    fn a_corrupt_hash_never_verifies() {
        assert!(!verify_password("anything", "not-a-phc-string"));
        assert!(!verify_password("anything", ""));
    }

    #[test]
    fn the_dummy_hash_is_usable_and_never_matches() {
        // If this stopped parsing, the unknown-email path would return early and become an
        // account-existence oracle.
        assert!(
            PasswordHash::new(dummy_hash()).is_ok(),
            "dummy hash must parse"
        );
        assert!(!verify_password("", dummy_hash()));
        assert!(!verify_password("password", dummy_hash()));
    }
}
