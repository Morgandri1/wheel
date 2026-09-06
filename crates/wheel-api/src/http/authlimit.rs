//! Rate limits for the unauthenticated auth endpoints.
//!
//! Counted in Postgres rather than in memory for the same reason the ingress limiter is: the API
//! runs as N replicas, and a per-replica bucket silently becomes N times the configured limit —
//! the control weakens exactly as you scale.
//!
//! Login is limited per **email**, not per source address. An IP limit alone misses the attack that
//! matters: a password spray from many addresses against one account. Limiting the account being
//! attacked is what actually protects it.

use crate::db::Db;
use crate::error::{ApiError, ApiResult};

#[derive(Clone)]
pub struct AuthLimiter {
    login_per_15min: i64,
    signups_per_hour: i64,
}

impl AuthLimiter {
    pub fn new(login_per_15min: i64, signups_per_hour: i64) -> Self {
        Self {
            login_per_15min,
            signups_per_hour,
        }
    }

    pub async fn check_login(&self, db: &Db, email: &str) -> ApiResult<()> {
        if self.login_per_15min <= 0 {
            return Ok(());
        }
        // Normalised so casing cannot be used to buy extra attempts.
        let key = format!("login:{}", email.trim().to_ascii_lowercase());
        let attempts = self.bump(db, &key, 15 * 60).await?;
        if attempts > self.login_per_15min {
            // Deliberately 429 rather than 401: the caller is being throttled, and pretending the
            // password was wrong would make the limit invisible to a legitimate user locked out by
            // someone else attacking their account.
            return Err(ApiError::RateLimited);
        }
        Ok(())
    }

    pub async fn check_signup(&self, db: &Db) -> ApiResult<()> {
        if self.signups_per_hour <= 0 {
            return Ok(());
        }
        let attempts = self.bump(db, "signup:global", 60 * 60).await?;
        if attempts > self.signups_per_hour {
            return Err(ApiError::RateLimited);
        }
        Ok(())
    }

    /// Count one attempt in the current window and return the running total.
    ///
    /// The window boundary is floor(epoch / width), computed from the *database* clock so replicas
    /// agree on where a window starts even when their own clocks differ by seconds.
    ///
    /// Fixed windows admit the usual boundary burst — up to twice the limit across two adjacent
    /// windows. Accepted: this exists to stop sustained guessing, not to shape traffic.
    async fn bump(&self, db: &Db, key: &str, window_secs: i64) -> ApiResult<i64> {
        // Both forms floor the database clock's epoch into a window of `$2` seconds. Written twice
        // because SQLite has neither `to_timestamp` nor `extract`, and computing the boundary in
        // Rust instead would put replica clock skew into a security control.
        const PG: &str = "INSERT INTO auth_attempts (key, window_start, attempts) \
             VALUES ($1, to_timestamp(floor(extract(epoch from now()) / $2::double precision) * $2::double precision), 1) \
             ON CONFLICT (key, window_start) \
             DO UPDATE SET attempts = auth_attempts.attempts + 1 \
             RETURNING attempts";
        const SQLITE: &str = "INSERT INTO auth_attempts (key, window_start, attempts) \
             VALUES ($1, CAST((CAST(strftime('%s','now') AS INTEGER) / $2) * $2 AS TEXT), 1) \
             ON CONFLICT (key, window_start) \
             DO UPDATE SET attempts = auth_attempts.attempts + 1 \
             RETURNING attempts";

        crate::db_scalar!(db, db.pick(PG, SQLITE), key, window_secs)
            .map_err(|e| ApiError::Internal(anyhow::Error::new(e).context("auth rate limit")))
    }
}

/// Drop counters for windows that have closed.
pub async fn sweep(db: &Db) -> anyhow::Result<u64> {
    const PG: &str = "DELETE FROM auth_attempts WHERE window_start < now() - interval '2 hours'";
    // The sqlite window_start is an epoch-second string, so the cutoff is one too.
    const SQLITE: &str =
        "DELETE FROM auth_attempts WHERE CAST(window_start AS INTEGER) < CAST(strftime('%s','now','-2 hours') AS INTEGER)";
    Ok(crate::db_execute!(db, db.pick(PG, SQLITE))?)
}
