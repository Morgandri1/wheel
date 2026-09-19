// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

//! Membership and invites.
//!
//! Only an admin manages either (operator ruling, 2026-09-11). A prompter cannot invite and cannot
//! raise their own tier — which falls out of `require(Admin)` rather than needing a rule about
//! self-modification, because a prompter never reaches any of these handlers.
//!
//! Reading the member list is a guest capability: seeing who else is in a project you belong to is
//! viewing. Invites are admin even to *list*, because an invite is a credential and its existence,
//! tier and expiry are facts about who is about to gain access.

use crate::auth::{AdminScope, ProjectScope, Tier};
use crate::error::{ApiError, ApiResult};
use crate::membership;
use crate::state::AppState;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Serialize)]
pub struct MemberList {
    /// The creator, who is always an admin and is never a `project_members` row. A principal —
    /// opaque under either auth mode — never masked: it identifies nothing about the person beyond
    /// "member of this project", the same as a database row id would.
    creator: String,
    /// Display email for the creator, resolved and (for a guest caller, except their own) masked
    /// the same way each member's `email` below is. See `display_email`.
    #[serde(skip_serializing_if = "Option::is_none")]
    creator_email: Option<String>,
    members: Vec<membership::Member>,
}

/// The best display email this deployment can honestly attach to `user_id`, or `None`.
///
/// Under `local` auth every member's principal IS a `users.id`, so any row's email is one lookup
/// away — not just the caller's own. Under `jwks` there is deliberately no local row for another
/// provider's principal at all (migration 0006), so the ONLY email the server can ever know for a
/// jwks member is the caller's own, carried on `AuthUser` from the token THIS request presented —
/// `None` for every other jwks row is not a gap to close, it is the honest limit of what the server
/// can know without inventing a second identity store.
///
/// Under `external` the limit is different but the answer is the same, and the arm below says why.
async fn display_email(state: &AppState, scope: &ProjectScope, user_id: &str) -> Option<String> {
    match state.cfg.auth_mode {
        crate::config::AuthMode::Local => {
            let id = Uuid::parse_str(user_id).ok()?;
            crate::auth::local::find_user(&state.db, &id)
                .await
                .ok()
                .flatten()
                .map(|u| u.email)
        }
        // `external` joins `jwks` rather than `local`, even though an external principal IS a
        // `users.id` and the lookup above would succeed. It would succeed with a LIE: an
        // auto-provisioned account's address is synthetic by construction
        // (`external-<uuid>@external.invalid`, `auth::local::create_external_user`), because the
        // provider's `email` claim must never become a Wheel account address. Displaying that row
        // would put a fabricated address in the roster. The provider's real claim is carried on
        // `AuthUser` from the token THIS request presented, so — as under `jwks` — the caller's own
        // is the only one the server can honestly show. (`external_identities.email` holds the
        // claim for the operator's identity listing; making the roster read it is a widening of who
        // sees whose address, so it is a decision, not a fallthrough.)
        crate::config::AuthMode::Jwks | crate::config::AuthMode::External => {
            if user_id == scope.user.id() {
                scope.user.email().map(str::to_string)
            } else {
                None
            }
        }
    }
}

/// `GET /v1/projects/{id}/members`
pub async fn list(
    State(state): State<AppState>,
    scope: ProjectScope,
) -> ApiResult<Json<MemberList>> {
    // No `require`: reaching here proved membership, and a guest may see who else is here — WHO,
    // not necessarily their exact email if a lower tier shouldn't see it. `user_id`/`creator` stay
    // raw at every tier (opaque principals, nothing to hide); `email` is masked for a guest caller
    // on every row but their own, server-side, the same reasoning `get_board`'s
    // `redact_credentials` already uses: client-side masking alone would not protect a caller
    // hitting this route directly.
    let mut members = membership::list(&state.db, &scope.project.id).await?;
    let creator_id = scope.project.owner_id.clone();
    let mut creator_email = display_email(&state, &scope, &creator_id).await;
    for member in &mut members {
        member.email = display_email(&state, &scope, &member.user_id).await;
    }

    if scope.tier == Tier::Guest {
        let caller = scope.user.id();
        if creator_id != caller {
            creator_email = creator_email.map(|e| wheel_core::mask_identifier(&e));
        }
        for member in &mut members {
            if member.user_id != caller {
                member.email = member.email.as_deref().map(wheel_core::mask_identifier);
            }
        }
    }

    Ok(Json(MemberList {
        creator: creator_id,
        creator_email,
        members,
    }))
}

#[derive(Deserialize)]
pub struct GrantMember {
    pub user_id: String,
    pub role: String,
}

/// `POST /v1/projects/{id}/members`
pub async fn grant(
    State(state): State<AppState>,
    AdminScope(scope): AdminScope,
    Json(body): Json<GrantMember>,
) -> ApiResult<(StatusCode, Json<membership::Member>)> {
    let tier = parse_tier(&body.role)?;
    let member = membership::grant(
        &state.db,
        &state.membership,
        &scope.project,
        scope.user.id(),
        body.user_id.trim(),
        tier,
    )
    .await?;
    Ok((StatusCode::CREATED, Json(member)))
}

/// `DELETE /v1/projects/{id}/members/{user_id}`
///
/// The path segment is a principal, not a uuid: under legacy `jwks` auth a principal is the
/// provider's `sub`, which is arbitrary text. It is validated as a principal rather than parsed.
pub async fn revoke(
    State(state): State<AppState>,
    scope: ProjectScope,
    Path((_project, user_id)): Path<(Uuid, String)>,
) -> ApiResult<StatusCode> {
    scope.require(Tier::Admin)?;
    if user_id == scope.project.owner_id {
        return Err(ApiError::Conflict(
            "the project's creator cannot be removed from their own project".into(),
        ));
    }
    match membership::revoke(&state.db, &state.membership, &scope.project.id, &user_id).await? {
        true => Ok(StatusCode::NO_CONTENT),
        false => Err(ApiError::NotFound),
    }
}

#[derive(Deserialize)]
pub struct NewInvite {
    pub role: String,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub expires_in_days: Option<i64>,
    #[serde(default)]
    pub max_uses: Option<i32>,
}

#[derive(Serialize)]
pub struct CreatedInvite {
    #[serde(flatten)]
    pub info: membership::InviteInfo,
    /// The only time the value is ever returned.
    pub token: String,
}

/// `POST /v1/projects/{id}/invites`
pub async fn create_invite(
    State(state): State<AppState>,
    AdminScope(scope): AdminScope,
    Json(body): Json<NewInvite>,
) -> ApiResult<(StatusCode, Json<CreatedInvite>)> {
    let tier = parse_tier(&body.role)?;
    let issued = membership::create_invite(
        &state.db,
        &scope.project.id,
        scope.user.id(),
        tier,
        body.email.as_deref(),
        body.expires_in_days,
        body.max_uses,
    )
    .await?;
    Ok((
        StatusCode::CREATED,
        Json(CreatedInvite {
            info: issued.info,
            token: issued.token,
        }),
    ))
}

/// `GET /v1/projects/{id}/invites`
pub async fn list_invites(
    State(state): State<AppState>,
    scope: ProjectScope,
) -> ApiResult<Json<Vec<membership::InviteInfo>>> {
    scope.require(Tier::Admin)?;
    Ok(Json(
        membership::list_invites(&state.db, &scope.project.id).await?,
    ))
}

/// `DELETE /v1/projects/{id}/invites/{invite_id}`
pub async fn revoke_invite(
    State(state): State<AppState>,
    scope: ProjectScope,
    Path((_project, invite_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<StatusCode> {
    scope.require(Tier::Admin)?;
    match membership::revoke_invite(&state.db, &scope.project.id, &invite_id).await? {
        true => Ok(StatusCode::NO_CONTENT),
        false => Err(ApiError::NotFound),
    }
}

#[derive(Deserialize)]
pub struct AcceptInvite {
    pub token: String,
}

#[derive(Serialize)]
pub struct Accepted {
    pub project_id: Uuid,
    pub role: Tier,
}

/// `POST /v1/invites/accept`
///
/// Takes `AuthUser`, not `ProjectScope`: the caller is not a member yet, which is the entire point.
/// The invite names the project, so there is nothing for the caller to choose.
pub async fn accept(
    State(state): State<AppState>,
    user: crate::auth::AuthUser,
    Json(body): Json<AcceptInvite>,
) -> ApiResult<Json<Accepted>> {
    // The lock is checked against the account's *verified* address, looked up here rather than
    // taken from the request. An address in a request body is a claim, not an identity.
    let email = match Uuid::parse_str(user.id()) {
        Ok(id) => crate::auth::local::find_user(&state.db, &id)
            .await?
            .map(|u| u.email),
        Err(_) => None,
    };
    let (project_id, role) = membership::accept(
        &state.db,
        &state.membership,
        body.token.trim(),
        user.id(),
        email.as_deref(),
    )
    .await?;
    Ok(Json(Accepted { project_id, role }))
}

/// Parse a tier from a request body.
///
/// Named separately so the error says what the three are. An unknown role is a 400 rather than a
/// silent default: there is no tier that is safe to assume someone meant.
fn parse_tier(raw: &str) -> ApiResult<Tier> {
    Tier::parse(raw.trim()).ok_or_else(|| {
        ApiError::BadRequest("role must be \"admin\", \"prompter\" or \"guest\"".into())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_role_must_be_one_of_the_three() {
        assert_eq!(parse_tier(" admin ").unwrap(), Tier::Admin);
        assert_eq!(parse_tier("prompter").unwrap(), Tier::Prompter);
        assert_eq!(parse_tier("guest").unwrap(), Tier::Guest);
        for bad in ["owner", "editor", "viewer", "root", "Admin", ""] {
            assert!(parse_tier(bad).is_err(), "{bad} must not parse");
        }
    }
}
