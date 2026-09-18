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
    /// The creator, who is always an admin and is never a `project_members` row.
    creator: String,
    members: Vec<membership::Member>,
}

/// `GET /v1/projects/{id}/members`
pub async fn list(
    State(state): State<AppState>,
    scope: ProjectScope,
) -> ApiResult<Json<MemberList>> {
    // No `require`: reaching here proved membership, and a guest may see who else is here.
    let members = membership::list(&state.db, &scope.project.id).await?;
    Ok(Json(MemberList {
        creator: scope.project.owner_id.clone(),
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
