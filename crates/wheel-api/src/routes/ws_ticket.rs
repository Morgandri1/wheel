//! Single-use tickets for browser WebSocket handshakes.
//!
//! # Why this exists
//!
//! Every other route authenticates with the `x-auth-token` header. A browser cannot set headers on
//! a WebSocket handshake — the `WebSocket` constructor takes a URL and nothing else — and the
//! session JWT must never be put in a URL, because URLs leak into proxy logs, `Referer` headers,
//! and browser history. So the client POSTs its JWT here over a normal authenticated request and
//! receives a ticket it may put in the query string exactly once.
//!
//! # What makes a ticket safe to expose in a URL
//!
//! * **Short-lived** — 30 seconds, per ARCHITECTURE §5. Long enough to open a socket, too short to
//!   be worth harvesting from a log.
//! * **Single-use** — redemption and validation happen in one atomic statement, so two replicas
//!   racing on the same ticket cannot both accept it.
//! * **Bound to (user, project)** — a ticket minted for one project cannot open another's socket,
//!   even for the same user.
//! * **Stored hashed** — the table holds SHA-256(ticket), so read access to the database yields
//!   nothing that can open a socket.

use crate::auth::ProjectScope;
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;
use axum::extract::State;
use axum::Json;
use base64::Engine as _;
use serde_json::json;
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// Lifetime of a ticket. ARCHITECTURE §5 pins this at 30 seconds.
const TICKET_TTL_SECS: i64 = 30;

pub fn hash_ticket(ticket: &str) -> Vec<u8> {
    Sha256::digest(ticket.as_bytes()).to_vec()
}

/// `POST /v1/projects/{id}/ws-ticket` → `{ ticket, expires_in }`.
///
/// Takes `ProjectScope`, so ownership is proven before a ticket exists: a ticket can only ever be
/// minted for a project the caller already owns.
pub async fn mint(
    State(state): State<AppState>,
    scope: ProjectScope,
) -> ApiResult<Json<serde_json::Value>> {
    let ticket = {
        use aes_gcm::aead::rand_core::RngCore;
        let mut buf = [0u8; 32];
        aes_gcm::aead::OsRng.fill_bytes(&mut buf);
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf)
    };

    sqlx::query(
        "INSERT INTO ws_tickets (ticket_hash, user_id, project_id, expires_at) \
         VALUES ($1, $2, $3, now() + make_interval(secs => $4))",
    )
    .bind(hash_ticket(&ticket))
    .bind(scope.user.id())
    .bind(scope.project.id)
    .bind(TICKET_TTL_SECS as f64)
    .execute(&state.db)
    .await?;

    // The ticket itself is returned exactly here and never logged.
    Ok(Json(
        json!({ "ticket": ticket, "expires_in": TICKET_TTL_SECS }),
    ))
}

/// Redeem a ticket for the project it was minted against.
///
/// Validation and consumption are a single statement on purpose. Checking "is it valid?" and then
/// separately marking it used would leave a window in which two connections — plausibly on two
/// different replicas — both pass the check. The `used_at IS NULL` predicate inside the `UPDATE`
/// makes the database the arbiter, so exactly one caller can win.
pub async fn redeem(state: &AppState, ticket: &str, project_id: &Uuid) -> ApiResult<String> {
    // Every condition — identity, freshness, single-use, and the project binding — is a predicate
    // in this one statement.
    //
    // The project match in particular belongs here rather than in a follow-up comparison. Checking
    // it afterwards would mean a ticket presented against the wrong project had already been
    // marked used by the time we rejected it, so anyone who could replay a ticket at the wrong
    // project could burn the legitimate owner's ticket. As a WHERE predicate, a mismatch simply
    // matches no row and consumes nothing.
    let row: Option<(String,)> = sqlx::query_as(
        "UPDATE ws_tickets SET used_at = now() \
         WHERE ticket_hash = $1 AND project_id = $2 AND used_at IS NULL AND expires_at > now() \
         RETURNING user_id",
    )
    .bind(hash_ticket(ticket))
    .bind(project_id)
    .fetch_optional(&state.db)
    .await?;

    // Unknown, expired, already redeemed, and wrong-project are deliberately indistinguishable.
    let Some((user_id,)) = row else {
        return Err(ApiError::Unauthorized(
            "ws ticket invalid, expired, already used, or for another project",
        ));
    };

    Ok(user_id)
}

/// Delete redeemed and expired tickets. Idempotent, so every replica may run it.
pub async fn sweep(db: &sqlx::PgPool) -> anyhow::Result<u64> {
    let r = sqlx::query("DELETE FROM ws_tickets WHERE expires_at < now() - interval '5 minutes'")
        .execute(db)
        .await?;
    Ok(r.rows_affected())
}
