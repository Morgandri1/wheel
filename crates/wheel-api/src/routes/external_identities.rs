// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

//! Linking an external subject to a Wheel account, and taking that link away.
//!
//! These are the operator's levers over external auth, and they are the only ones Wheel has: there
//! is no back-channel logout and no SCIM in v1, so a user deleted at the provider is invisible here
//! until somebody disables their link.
//!
//! Guarded the same way `POST /v1/auth/users` is — the token-only owner account, the one `wheeld`
//! creates on first boot and that no signup can produce. That account is also what makes
//! `WHEEL_EXTERNAL_PROVISION=linked` usable from nothing: it is the pre-existing principal that
//! performs the first link.

use crate::auth::external::{self, ExternalIdentity};
use crate::auth::AuthUser;
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use uuid::Uuid;

/// Only the operator account may see or change who is who.
async fn require_operator(state: &AppState, user: &AuthUser) -> ApiResult<()> {
    let id = Uuid::parse_str(user.id())
        .map_err(|_| ApiError::Forbidden("only the owner account may manage identities"))?;
    match crate::auth::local::is_token_only(&state.db, &id).await? {
        true => Ok(()),
        false => Err(ApiError::Forbidden(
            "only the owner account may manage identities",
        )),
    }
}

fn external_cfg(state: &AppState) -> ApiResult<&crate::config::ExternalAuth> {
    state.cfg.external.as_ref().ok_or(ApiError::NotFound)
}

/// `GET /v1/auth/external-identities`
pub async fn list(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<Vec<Identity>>> {
    external_cfg(&state)?;
    require_operator(&state, &user).await?;
    Ok(Json(
        external::list_all(&state.db)
            .await?
            .into_iter()
            .map(Identity::from)
            .collect(),
    ))
}

#[derive(Deserialize)]
pub struct LinkIdentity {
    /// The provider's subject, exactly as it appears in the claim named by
    /// `WHEEL_EXTERNAL_SUBJECT_CLAIM`.
    pub subject: String,
    /// The Wheel account it belongs to.
    pub user_id: Uuid,
    #[serde(default)]
    pub email: Option<String>,
}

/// `POST /v1/auth/external-identities`
///
/// The issuer is **not** a parameter: it is taken from configuration, because it is the thing that
/// was or will be cryptographically asserted. Letting a caller name one would mean an operator
/// could link a subject under an issuer this deployment never verifies, creating a row that can
/// never match and looks like it should.
pub async fn link(
    State(state): State<AppState>,
    user: AuthUser,
    Json(body): Json<LinkIdentity>,
) -> ApiResult<(StatusCode, Json<Identity>)> {
    let ext = external_cfg(&state)?.clone();
    require_operator(&state, &user).await?;

    let subject = body.subject.trim();
    crate::auth::principal::validate(subject)
        .map_err(|e| ApiError::BadRequest(format!("subject is not a usable principal: {e}")))?;
    // The account has to exist. Linking to an absent one would create a row that authenticates
    // nobody, and would do it silently.
    crate::auth::local::find_user(&state.db, &body.user_id)
        .await?
        .ok_or(ApiError::NotFound)?;

    let verified = external::Verified {
        subject: subject.to_string(),
        email: body.email.clone(),
    };
    let row = external::link(&state.db, &ext, &verified, body.user_id, Some(user.id())).await?;
    Ok((StatusCode::CREATED, Json(Identity::from(row))))
}

/// `DELETE /v1/auth/external-identities/{id}`
///
/// Soft: the link stays visible afterwards, and re-enabling is a decision rather than a fresh
/// provision under a new Wheel account — which would silently orphan the old account's projects.
pub async fn disable(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    external_cfg(&state)?;
    require_operator(&state, &user).await?;
    match external::disable(&state.db, &id).await? {
        true => Ok(StatusCode::NO_CONTENT),
        false => Err(ApiError::NotFound),
    }
}

/// What an operator sees. Everything on the row: none of it is a credential.
#[derive(serde::Serialize)]
pub struct Identity {
    pub id: Uuid,
    pub provider: String,
    pub issuer: String,
    pub subject: String,
    pub user_id: Uuid,
    pub email: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub last_seen_at: Option<chrono::DateTime<chrono::Utc>>,
    pub disabled_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl From<ExternalIdentity> for Identity {
    fn from(r: ExternalIdentity) -> Self {
        Identity {
            id: r.id,
            provider: r.provider,
            issuer: r.issuer,
            subject: r.subject,
            user_id: r.user_id,
            email: r.email,
            created_at: r.created_at,
            last_seen_at: r.last_seen_at,
            disabled_at: r.disabled_at,
        }
    }
}
