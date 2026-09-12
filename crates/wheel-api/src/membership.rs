// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

//! Membership, invites, and telling live connections when either changes.
//!
//! # Why revocation needs more than a table
//!
//! Every other authorisation check in this API runs per request, so revoking a member takes effect
//! on their next one. A member watching `/v1/events` holds a WebSocket that will never make
//! another request, and no future check runs against it. Closing it is a separate problem, and it
//! is the reason [`MembershipEvents`] exists.
//!
//! Three controls, layered, because each covers what the others cannot:
//!
//!   1. **NOTIFY/LISTEN** (this module) closes a bridge within milliseconds, across replicas.
//!   2. **A periodic re-check on the bridge** (`routes::proxy`) closes it even if the notification
//!      was missed — a dropped listener, a row changed by hand, a backend with no NOTIFY at all.
//!   3. **An absolute lifetime cap** bounds how long a missed revocation can persist even if both
//!      of the above fail.
//!
//! Control 1 makes revocation *fast*. Control 2 makes it *certain*. Shipping only the first would
//! be the more impressive-looking half and the wrong one to rely on.

use crate::auth::Tier;
use crate::db::Db;
use crate::error::{ApiError, ApiResult};
use base64::Engine as _;
use chrono::{DateTime, Utc};
use serde::Serialize;
use sha2::Digest as _;
use uuid::Uuid;

/// The Postgres channel. One channel for every project, with the project in the payload: a channel
/// per project would mean issuing `LISTEN` as projects are created, and a replica that missed one
/// would silently stop revoking for it.
pub const CHANNEL: &str = "wheel_membership";

/// "This principal's access to this project changed — anything open should re-check."
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessChanged {
    pub project_id: Uuid,
    pub user_id: String,
}

impl AccessChanged {
    /// `<project uuid>:<principal>`.
    ///
    /// Split on the *first* colon, because a principal may contain one (`principal.rs` permits it)
    /// while a uuid may not. Parsing from the left is therefore unambiguous, and parsing from the
    /// right would not be.
    pub fn encode(&self) -> String {
        format!("{}:{}", self.project_id, self.user_id)
    }

    pub fn decode(raw: &str) -> Option<Self> {
        let (project, user) = raw.split_once(':')?;
        let project_id = Uuid::parse_str(project).ok()?;
        // A payload that is not a principal cannot have come from us. Refusing it keeps a hostile
        // or corrupt NOTIFY from becoming a way to steer which sockets get closed.
        crate::auth::principal::validate(user).ok()?;
        Some(AccessChanged {
            project_id,
            user_id: user.to_string(),
        })
    }
}

/// Fan-out of access changes to whatever is holding a connection open.
///
/// One implementation, two sources: a Postgres `LISTEN` task feeds it on the deployed store, and
/// on SQLite the publisher feeds it directly. SQLite is complete this way rather than degraded —
/// `wheeld` is a single process, so there is no second replica for a notification to reach.
#[derive(Clone)]
pub struct MembershipEvents {
    tx: tokio::sync::broadcast::Sender<AccessChanged>,
}

impl Default for MembershipEvents {
    fn default() -> Self {
        Self::new()
    }
}

impl MembershipEvents {
    pub fn new() -> Self {
        // Capacity is generous relative to how often memberships change. A lagged receiver is not
        // a correctness problem here: the bridge's periodic re-check is what guarantees closure,
        // and a missed notification only costs latency.
        let (tx, _) = tokio::sync::broadcast::channel(256);
        Self { tx }
    }

    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<AccessChanged> {
        self.tx.subscribe()
    }

    /// Deliver to this process's subscribers. Returns how many received it.
    pub fn publish(&self, change: AccessChanged) -> usize {
        self.tx.send(change).unwrap_or(0)
    }
}

/// Announce that a principal's access to a project changed.
///
/// On Postgres the notification is issued in the same statement batch as the row change's
/// transaction has committed, so a replica cannot be told before the change is visible. On SQLite
/// it goes straight to the in-process bus.
pub async fn announce(db: &Db, events: &MembershipEvents, change: AccessChanged) {
    match db.dialect() {
        crate::db::Dialect::Postgres => {
            // pg_notify rather than the NOTIFY statement: the channel and payload are bound
            // parameters, so neither can be injected by a principal that reached this far.
            let sent = crate::db_execute!(db, "SELECT pg_notify($1, $2)", CHANNEL, change.encode());
            if let Err(e) = sent {
                // Not fatal. The bridge's periodic re-check still closes the connection; this only
                // costs latency, and failing the revocation itself would be worse.
                tracing::warn!(error = ?e, "membership NOTIFY failed; relying on the periodic re-check");
            }
        }
        crate::db::Dialect::Sqlite => {
            events.publish(change);
        }
    }
}

/// Feed [`MembershipEvents`] from Postgres `LISTEN`, so a revocation reaches **every** replica.
///
/// Returns `None` on SQLite, where there is nothing to listen for: `announce` publishes straight to
/// the in-process bus, which is complete when there is one process.
///
/// # If this task dies, revocation still works
///
/// It reconnects with a backoff, and every failure is logged — but the thing that makes it safe to
/// have a task that can die at all is that it is not the guarantee. The bridge's periodic re-check
/// is. Losing this listener costs revocation *latency* (up to one re-check interval), not
/// correctness, which is why it fails loudly and carries on rather than taking the process down.
#[cfg(feature = "postgres")]
pub fn spawn_listener(db: &Db, events: MembershipEvents) -> Option<tokio::task::JoinHandle<()>> {
    let pool = db.as_pg()?.clone();
    Some(tokio::spawn(async move {
        // Bounded backoff: a database that is briefly unreachable must not become a reconnect
        // storm aimed at it, and one that is gone for a while must not stop trying.
        let mut backoff = std::time::Duration::from_millis(250);
        const MAX_BACKOFF: std::time::Duration = std::time::Duration::from_secs(30);

        loop {
            match listen_once(&pool, &events).await {
                Ok(()) => {
                    tracing::warn!("membership listener ended cleanly; reconnecting");
                    backoff = std::time::Duration::from_millis(250);
                }
                Err(e) => tracing::error!(
                    error = ?e,
                    "membership listener failed; revocation falls back to the periodic re-check \
                     until it reconnects"
                ),
            }
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(MAX_BACKOFF);
        }
    }))
}

#[cfg(feature = "postgres")]
async fn listen_once(pool: &sqlx::PgPool, events: &MembershipEvents) -> anyhow::Result<()> {
    let mut listener = sqlx::postgres::PgListener::connect_with(pool).await?;
    listener.listen(CHANNEL).await?;
    tracing::info!(channel = CHANNEL, "listening for membership changes");
    loop {
        let notification = listener.recv().await?;
        match AccessChanged::decode(notification.payload()) {
            Some(change) => {
                events.publish(change);
            }
            // A payload we could not have written. Logged rather than dropped silently, because
            // the only ways to produce one are a bug on our side or somebody else issuing NOTIFY
            // on our channel, and both are worth seeing.
            None => tracing::warn!(
                payload = notification.payload(),
                "discarding an unreadable membership notification"
            ),
        }
    }
}

/// No Postgres in this build, so there is nothing to listen to.
#[cfg(not(feature = "postgres"))]
pub fn spawn_listener(_db: &Db, _events: MembershipEvents) -> Option<tokio::task::JoinHandle<()>> {
    None
}

// ---------------------------------------------------------------------------- members

#[derive(Debug, Clone, Serialize)]
pub struct Member {
    pub user_id: String,
    pub role: Tier,
    pub invited_by: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(sqlx::FromRow)]
struct MemberRow {
    user_id: String,
    role: String,
    invited_by: Option<String>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

/// Live members of a project, creator excluded — the creator is `projects.owner_id` and is not a
/// row here (migration 0006 explains why). Callers that render a member list add the creator.
pub async fn list(db: &Db, project_id: &Uuid) -> ApiResult<Vec<Member>> {
    let rows: Vec<MemberRow> = crate::db_fetch_all!(
        db,
        "SELECT user_id, role, invited_by, created_at, updated_at FROM project_members \
         WHERE project_id = $1 AND revoked_at IS NULL ORDER BY created_at, user_id",
        project_id
    )?;
    Ok(rows
        .into_iter()
        .filter_map(|r| {
            // A role this build cannot parse is not shown as a role. Same fail-closed choice as
            // `load_member`: there is no safe direction to round an unknown tier.
            Tier::parse(&r.role).map(|role| Member {
                user_id: r.user_id,
                role,
                invited_by: r.invited_by,
                created_at: r.created_at,
                updated_at: r.updated_at,
            })
        })
        .collect())
}

/// Grant or change a member's tier.
///
/// Refuses the project's creator outright. The creator's admin comes from `projects.owner_id`, and
/// a row here saying anything else would be the second source of truth that migration 0006 exists
/// to prevent — it could not demote them (`load_member` reads `owner_id` first), so it could only
/// mislead whoever read the table.
pub async fn grant(
    db: &Db,
    events: &MembershipEvents,
    project: &crate::models::Project,
    actor: &str,
    user_id: &str,
    tier: Tier,
) -> ApiResult<Member> {
    crate::auth::principal::validate(user_id)
        .map_err(|e| ApiError::BadRequest(format!("member id is not a usable principal: {e}")))?;
    if user_id == project.owner_id {
        return Err(ApiError::Conflict(
            "the project's creator is always an admin and cannot be given a role".into(),
        ));
    }

    let now = db.pick("now()", "strftime('%Y-%m-%dT%H:%M:%fZ', 'now')");
    // Re-granting a revoked membership revives that row rather than leaving a tombstone that would
    // shadow the new grant on the primary key.
    let sql = format!(
        "INSERT INTO project_members (project_id, user_id, role, invited_by) \
         VALUES ($1, $2, $3, $4) \
         ON CONFLICT (project_id, user_id) DO UPDATE \
            SET role = $3, invited_by = $4, updated_at = {now}, revoked_at = NULL"
    );
    crate::db_execute!(db, &sql, project.id, user_id, tier.as_str(), actor)?;

    // Announced on every change, not only on revocation: a downgrade has to reach a live
    // connection too, or a demoted admin keeps an admin-tier socket until it happens to close.
    announce(
        db,
        events,
        AccessChanged {
            project_id: project.id,
            user_id: user_id.to_string(),
        },
    )
    .await;

    find(db, &project.id, user_id)
        .await?
        .ok_or_else(|| ApiError::Internal(anyhow::anyhow!("granted membership vanished")))
}

pub async fn find(db: &Db, project_id: &Uuid, user_id: &str) -> ApiResult<Option<Member>> {
    let row: Option<MemberRow> = crate::db_fetch_optional!(
        db,
        "SELECT user_id, role, invited_by, created_at, updated_at FROM project_members \
         WHERE project_id = $1 AND user_id = $2 AND revoked_at IS NULL",
        project_id,
        user_id
    )?;
    Ok(row.and_then(|r| {
        Tier::parse(&r.role).map(|role| Member {
            user_id: r.user_id,
            role,
            invited_by: r.invited_by,
            created_at: r.created_at,
            updated_at: r.updated_at,
        })
    }))
}

/// Whether this principal's membership of this project was **revoked**.
///
/// Distinct from [`find`], which only sees live rows. Revocation is a decision about a person, and
/// the soft-deleted row is the record of that decision — so it has to be readable by the one place
/// that would otherwise overturn it.
pub async fn was_revoked(db: &Db, project_id: &Uuid, user_id: &str) -> ApiResult<bool> {
    let n: i64 = crate::db_scalar!(
        db,
        "SELECT count(*) FROM project_members \
         WHERE project_id = $1 AND user_id = $2 AND revoked_at IS NOT NULL",
        project_id,
        user_id
    )?;
    Ok(n > 0)
}

/// End a membership. Soft, so it stays visible, and so a live connection has something to react to.
pub async fn revoke(
    db: &Db,
    events: &MembershipEvents,
    project_id: &Uuid,
    user_id: &str,
) -> ApiResult<bool> {
    let now = db.pick("now()", "strftime('%Y-%m-%dT%H:%M:%fZ', 'now')");
    let sql = format!(
        "UPDATE project_members SET revoked_at = COALESCE(revoked_at, {now}), updated_at = {now} \
         WHERE project_id = $1 AND user_id = $2 AND revoked_at IS NULL"
    );
    let changed = crate::db_execute!(db, &sql, project_id, user_id)?;
    if changed > 0 {
        announce(
            db,
            events,
            AccessChanged {
                project_id: *project_id,
                user_id: user_id.to_string(),
            },
        )
        .await;
    }
    Ok(changed > 0)
}

// ---------------------------------------------------------------------------- invites

/// An invite token: `wi_` and 32 random bytes, base64url. The `wht_` shape, for the `wht_` reasons.
pub const INVITE_PREFIX: &str = "wi_";
const DEFAULT_TTL_DAYS: i64 = 7;

fn invite_digest(token: &str) -> String {
    hex::encode(sha2::Sha256::digest(token.as_bytes()))
}

fn generate_invite() -> String {
    use rand::RngCore;
    let mut secret = [0u8; 32];
    rand::rng().fill_bytes(&mut secret);
    format!(
        "{INVITE_PREFIX}{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(secret)
    )
}

/// What an admin may see of an invite: never the token, never its hash.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct InviteInfo {
    pub id: Uuid,
    pub project_id: Uuid,
    pub role: String,
    pub email: Option<String>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub max_uses: i32,
    pub uses: i32,
    pub revoked_at: Option<DateTime<Utc>>,
}

const INVITE_COLUMNS: &str = "id, project_id, role, email, created_by, created_at, expires_at, \
                              max_uses, uses, revoked_at";

pub struct IssuedInvite {
    pub info: InviteInfo,
    /// The only moment anything holds this value.
    pub token: String,
}

pub async fn create_invite(
    db: &Db,
    project_id: &Uuid,
    created_by: &str,
    tier: Tier,
    email: Option<&str>,
    ttl_days: Option<i64>,
    max_uses: Option<i32>,
) -> ApiResult<IssuedInvite> {
    let ttl = ttl_days.unwrap_or(DEFAULT_TTL_DAYS);
    if !(1..=90).contains(&ttl) {
        return Err(ApiError::BadRequest(
            "invite lifetime must be between 1 and 90 days".into(),
        ));
    }
    let max_uses = max_uses.unwrap_or(1);
    if !(1..=100).contains(&max_uses) {
        return Err(ApiError::BadRequest(
            "invite use count must be between 1 and 100".into(),
        ));
    }
    let email = match email.map(str::trim).filter(|s| !s.is_empty()) {
        Some(e) => Some(crate::auth::local::validate_email(e).map_err(ApiError::BadRequest)?),
        None => None,
    };

    let id = Uuid::new_v4();
    let token = generate_invite();
    // Expiry is computed by the database on both backends, for the reason ws-tickets record: an
    // invite created against one replica and redeemed against another must agree on when it dies,
    // and the only clock both see is the database's.
    const PG: &str = "INSERT INTO project_invites \
         (id, project_id, role, token_hash, email, created_by, expires_at, max_uses) \
         VALUES ($1, $2, $3, $4, $5, $6, now() + make_interval(days => $7::int), $8)";
    const SQLITE: &str = "INSERT INTO project_invites \
         (id, project_id, role, token_hash, email, created_by, expires_at, max_uses) \
         VALUES ($1, $2, $3, $4, $5, $6, \
                 strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '+' || $7 || ' days'), $8)";

    crate::db_execute!(
        db,
        db.pick(PG, SQLITE),
        id,
        project_id,
        tier.as_str(),
        invite_digest(&token),
        email.as_deref(),
        created_by,
        ttl as i32,
        max_uses
    )?;

    let info: InviteInfo = crate::db_fetch_one!(
        db,
        &format!("SELECT {INVITE_COLUMNS} FROM project_invites WHERE id = $1"),
        id
    )?;
    Ok(IssuedInvite { info, token })
}

pub async fn list_invites(db: &Db, project_id: &Uuid) -> ApiResult<Vec<InviteInfo>> {
    Ok(crate::db_fetch_all!(
        db,
        &format!(
            "SELECT {INVITE_COLUMNS} FROM project_invites WHERE project_id = $1 \
             ORDER BY created_at DESC, id"
        ),
        project_id
    )?)
}

pub async fn revoke_invite(db: &Db, project_id: &Uuid, id: &Uuid) -> ApiResult<bool> {
    let now = db.pick("now()", "strftime('%Y-%m-%dT%H:%M:%fZ', 'now')");
    // Scoped by project as a predicate rather than checked after the fetch, so an admin of one
    // project cannot burn another project's invite by id.
    let sql = format!(
        "UPDATE project_invites SET revoked_at = COALESCE(revoked_at, {now}) \
         WHERE id = $1 AND project_id = $2"
    );
    Ok(crate::db_execute!(db, &sql, id, project_id)? > 0)
}

/// Redeem an invite for the calling principal.
///
/// Every condition — existence, expiry, revocation and the use count — is a predicate in one
/// `UPDATE ... RETURNING`, for the reason ws-tickets record: checking and then separately
/// consuming leaves a window in which two callers, plausibly on two replicas, both pass.
pub async fn accept(
    db: &Db,
    events: &MembershipEvents,
    token: &str,
    user_id: &str,
    user_email: Option<&str>,
) -> ApiResult<(Uuid, Tier)> {
    // Look first, consume second.
    //
    // This used to be one `UPDATE ... uses = uses + 1 ... RETURNING`, which read beautifully and
    // was wrong: the email lock and the role parse happened *after* the increment had committed, so
    // anyone who opened a forwarded link burned it. With the documented default of one use, the
    // person it was actually for was then told the invite was unknown or already used — a denial of
    // service any authenticated stranger could perform by clicking.
    //
    // The consumption below is still a single atomic statement with `uses < max_uses` as a
    // predicate, so two callers racing cannot both win. What moved is only the checks that must not
    // cost a use.
    const FIND_PG: &str = "SELECT project_id, role, email FROM project_invites \
         WHERE token_hash = $1 AND revoked_at IS NULL AND expires_at > now() AND uses < max_uses";
    const FIND_SQLITE: &str = "SELECT project_id, role, email FROM project_invites \
         WHERE token_hash = $1 AND revoked_at IS NULL \
           AND expires_at > strftime('%Y-%m-%dT%H:%M:%fZ', 'now') AND uses < max_uses";

    let digest = invite_digest(token);
    let row: Option<(Uuid, String, Option<String>)> =
        crate::db_fetch_optional!(db, db.pick(FIND_PG, FIND_SQLITE), &digest)?;

    // Unknown, expired, revoked and exhausted are deliberately one answer: an invite link is a
    // credential, and distinguishing them would say which links exist.
    let unusable = || ApiError::Unauthorized("invite is unknown, expired, revoked, or fully used");
    let Some((project_id, role, locked_email)) = row else {
        return Err(unusable());
    };
    let tier = Tier::parse(&role).ok_or(ApiError::Unauthorized("invite has an unknown role"))?;

    // The lock is checked against the *verified* account's address, never against anything in the
    // request. An email the caller supplies is a claim, not an identity.
    if let Some(want) = &locked_email {
        let matches = user_email.is_some_and(|have| have.eq_ignore_ascii_case(want));
        if !matches {
            return Err(ApiError::Unauthorized(
                "invite is locked to another address",
            ));
        }
    }

    let project: crate::models::ProjectRow = crate::db_fetch_one!(
        db,
        "SELECT id, owner_id, name, capabilities, status, created_at, updated_at \
         FROM projects WHERE id = $1",
        project_id
    )?;
    let project = crate::models::Project::from(project);

    // The creator already has admin from `projects.owner_id`. Writing a row for them would be the
    // second source of truth; accepting is a no-op, and a no-op does not spend a use.
    if project.owner_id == user_id {
        return Ok((project_id, Tier::Admin));
    }

    // **An admin's revocation outranks a link.** Without this, removing somebody was cosmetic: the
    // token they joined with still redeemed and restored the role, so a revoked member could walk
    // back in as often as they liked. Refusing here rather than revoking the invite is the narrower
    // fix — an invite may have been sent to several people, and one person's removal should not
    // cancel everyone else's.
    //
    // Re-admission is deliberately an explicit act: `POST /v1/projects/{id}/members` with the same
    // user id the revocation named. An admin who changed their mind says so, rather than a link
    // they no longer remember saying it for them.
    if was_revoked(db, &project_id, user_id).await? {
        return Err(ApiError::Unauthorized(
            "membership of this project was revoked; an admin must grant it again",
        ));
    }

    // Never lower an existing tier. Otherwise a stale guest link becomes a way to demote a
    // prompter — a downgrade that anyone holding an old link could trigger. Nothing changes, so
    // nothing is spent.
    if let Some(existing) = find(db, &project_id, user_id).await? {
        if existing.role >= tier {
            return Ok((project_id, existing.role));
        }
    }

    // Now consume, atomically. A caller that lost a race to the last use sees the same flat answer
    // as one whose link never existed.
    const USE_PG: &str = "UPDATE project_invites SET uses = uses + 1 \
         WHERE token_hash = $1 AND revoked_at IS NULL AND expires_at > now() AND uses < max_uses \
         RETURNING id";
    const USE_SQLITE: &str = "UPDATE project_invites SET uses = uses + 1 \
         WHERE token_hash = $1 AND revoked_at IS NULL \
           AND expires_at > strftime('%Y-%m-%dT%H:%M:%fZ', 'now') AND uses < max_uses \
         RETURNING id";
    let consumed: Option<(Uuid,)> =
        crate::db_fetch_optional!(db, db.pick(USE_PG, USE_SQLITE), &digest)?;
    if consumed.is_none() {
        return Err(unusable());
    }

    grant(db, events, &project, "invite", user_id, tier).await?;
    Ok((project_id, tier))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_access_change_round_trips_through_the_notify_payload() {
        let id = Uuid::new_v4();
        let c = AccessChanged {
            project_id: id,
            user_id: "alice".into(),
        };
        assert_eq!(AccessChanged::decode(&c.encode()), Some(c));
    }

    /// A principal may contain a colon, so the payload must split on the first one. Splitting on
    /// the last would truncate the principal and close the wrong sockets — or none.
    #[test]
    fn a_principal_containing_a_colon_survives_the_payload() {
        let id = Uuid::new_v4();
        let c = AccessChanged {
            project_id: id,
            user_id: "https://issuer.example/users/7".into(),
        };
        let decoded = AccessChanged::decode(&c.encode()).unwrap();
        assert_eq!(decoded.user_id, "https://issuer.example/users/7");
        assert_eq!(decoded.project_id, id);
    }

    #[test]
    fn a_payload_we_could_not_have_written_is_refused() {
        assert_eq!(AccessChanged::decode("not-a-uuid:alice"), None);
        assert_eq!(AccessChanged::decode("alice"), None);
        assert_eq!(AccessChanged::decode(""), None);
        // A principal that would not have passed validation cannot have come from us.
        let id = Uuid::new_v4();
        assert_eq!(AccessChanged::decode(&format!("{id}:alice bob")), None);
        assert_eq!(AccessChanged::decode(&format!("{id}:")), None);
    }

    #[test]
    fn an_invite_token_is_the_prefix_and_256_bits() {
        let t = generate_invite();
        assert!(t.starts_with(INVITE_PREFIX), "{t}");
        let secret = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(&t[INVITE_PREFIX.len()..])
            .unwrap();
        assert_eq!(secret.len(), 32);
        assert_ne!(generate_invite(), t, "two invites were identical");
    }

    #[test]
    fn the_stored_digest_is_not_the_token() {
        let d = invite_digest("wi_example");
        assert_eq!(d.len(), 64);
        assert!(d.chars().all(|c| c.is_ascii_hexdigit()));
        assert!(!d.contains("wi_"));
    }

    #[test]
    fn the_bus_delivers_to_a_subscriber_and_tolerates_none() {
        let ev = MembershipEvents::new();
        let change = AccessChanged {
            project_id: Uuid::new_v4(),
            user_id: "alice".into(),
        };
        // Nobody listening is not an error: publishing must never fail a revocation.
        assert_eq!(ev.publish(change.clone()), 0);

        let mut rx = ev.subscribe();
        assert_eq!(ev.publish(change.clone()), 1);
        assert_eq!(rx.try_recv().unwrap(), change);
    }
}
