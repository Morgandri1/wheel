// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

//! The caller's own API tokens, under every `AUTH_MODE`.
//!
//! A desktop client or a script holds one of these instead of a browser session. Any credential of
//! the account may mint one, including another token; revocation follows that lineage (see
//! `auth::api_token`), and minting is rate limited per account.

use crate::auth::api_token::{self, TokenInfo};
use crate::auth::{AuthUser, Credential};
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Deserialize)]
pub struct NewToken {
    pub name: String,
}

#[derive(Serialize)]
pub struct CreatedToken {
    pub id: Uuid,
    pub name: String,
    /// The only time the value is ever returned.
    pub token: String,
    pub created_at: String,
}

pub async fn create(
    State(state): State<AppState>,
    user: AuthUser,
    Json(body): Json<NewToken>,
) -> ApiResult<(StatusCode, Json<CreatedToken>)> {
    state.auth_limiter.check_mint(&state.db, user.id()).await?;
    let mint = match user.credential() {
        Credential::ApiToken(parent) => api_token::Mint::Token(parent),
        Credential::Session(session) => api_token::Mint::Session(session),
    };
    let issued = api_token::issue(&state.db, user.id(), &body.name, mint).await?;
    Ok((
        StatusCode::CREATED,
        Json(CreatedToken {
            id: issued.id,
            name: issued.name,
            token: issued.token,
            created_at: issued.created_at.to_rfc3339(),
        }),
    ))
}

pub async fn list(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<Vec<TokenInfo>>> {
    Ok(Json(api_token::list_for_user(&state.db, user.id()).await?))
}

/// `404` for a token that is not the caller's, exactly as for one that does not exist.
pub async fn revoke(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    let id = Uuid::parse_str(&id).map_err(|_| ApiError::NotFound)?;
    if api_token::revoke(&state.db, &id, Some(user.id())).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound)
    }
}
