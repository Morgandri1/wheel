//! Per-project ingress rate limiting, shared across API replicas.
//!
//! An in-memory bucket would be wrong here: with N replicas behind a load balancer each keeps its
//! own count, so a configured 60/min silently becomes N*60/min — the limit quietly weakens as we
//! scale, which is the opposite of what an abuse control should do. Postgres already exists in this
//! deployment, so it is the cheapest correct shared counter available.
//!
//! This is a **fixed window**, which admits the classic boundary burst (up to 2x the limit across
//! two adjacent windows). That is an accepted tradeoff for v1: this control exists to stop
//! sustained abuse of an unauthenticated public route, not to smooth traffic. A sliding window or
//! token bucket in redis is the upgrade path if we ever need precision.

use crate::error::{ApiError, ApiResult};
use uuid::Uuid;

#[derive(Clone)]
pub struct RateLimiter {
    limit_per_min: i64,
}

impl RateLimiter {
    pub fn new(limit_per_min: u32) -> Self {
        Self {
            limit_per_min: limit_per_min as i64,
        }
    }

    /// Count this request and reject it if the project is over budget for the current minute.
    pub async fn check(&self, db: &sqlx::PgPool, project_id: &Uuid) -> ApiResult<()> {
        if self.limit_per_min <= 0 {
            return Ok(()); // 0 disables the limit (documented in API.md)
        }

        // `date_trunc` on the server's clock, not ours: replicas must agree on window boundaries,
        // and their local clocks may differ by seconds.
        let hits: i64 = sqlx::query_scalar(
            "INSERT INTO ingress_rate_limits (project_id, window_start, hits) \
             VALUES ($1, date_trunc('minute', now()), 1) \
             ON CONFLICT (project_id, window_start) \
             DO UPDATE SET hits = ingress_rate_limits.hits + 1 \
             RETURNING hits",
        )
        .bind(project_id)
        .fetch_one(db)
        .await
        .map_err(|e| ApiError::Internal(anyhow::Error::new(e).context("rate limit counter")))?;

        if hits > self.limit_per_min {
            tracing::warn!(%project_id, hits, "ingress rate limit exceeded");
            return Err(ApiError::RateLimited);
        }
        Ok(())
    }
}

/// Drop counter rows for windows that have closed. Cheap, and keeps the table from growing without
/// bound. Called from the periodic maintenance task.
pub async fn sweep(db: &sqlx::PgPool) -> anyhow::Result<u64> {
    let r = sqlx::query(
        "DELETE FROM ingress_rate_limits WHERE window_start < now() - interval '10 minutes'",
    )
    .execute(db)
    .await?;
    Ok(r.rows_affected())
}
