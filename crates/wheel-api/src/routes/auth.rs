//! Local authentication routes (`AUTH_MODE=local`).
//!
//! Everything here is shaped around one idea: an unauthenticated endpoint that talks to the user
//! table is an oracle unless it is written not to be. Signup, login and password change all take
//! care to reveal the same thing whether or not an account exists.

use crate::auth::local;
use crate::auth::AuthUser;
use crate::config::AuthMode;
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;
use axum::extract::State;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

#[derive(Deserialize)]
pub struct Credentials {
    pub email: String,
    pub password: String,
}

#[derive(Deserialize)]
pub struct ChangePassword {
    pub current_password: String,
    pub new_password: String,
}

#[derive(Serialize)]
pub struct SessionResponse {
    pub token: String,
    pub expires_at: String,
    pub user: local::User,
}

/// Reject local-auth routes when the deployment is not using local auth, so a provider swap cannot
/// leave a second way in.
fn require_local(state: &AppState) -> ApiResult<()> {
    if state.cfg.auth_mode != AuthMode::Local {
        return Err(ApiError::NotFound);
    }
    Ok(())
}

fn issuer(state: &AppState) -> String {
    state.cfg.public_base_url.clone()
}

pub async fn signup(
    State(state): State<AppState>,
    Json(body): Json<Credentials>,
) -> ApiResult<(axum::http::StatusCode, Json<SessionResponse>)> {
    require_local(&state)?;
    state.auth_limiter.check_signup(&state.db).await?;

    let user = local::create_user(&state.db, &body.email, &body.password).await?;
    let session = local::issue_session(
        &state.db,
        &user.id,
        state.cfg.session_secret.expose(),
        &issuer(&state),
    )
    .await?;

    Ok((
        axum::http::StatusCode::CREATED,
        Json(SessionResponse {
            token: session.token,
            expires_at: session.expires_at.to_rfc3339(),
            user,
        }),
    ))
}

pub async fn login(
    State(state): State<AppState>,
    Json(body): Json<Credentials>,
) -> ApiResult<Json<SessionResponse>> {
    require_local(&state)?;
    // Rate limited on the email as well as the source, because a password-spray from many
    // addresses against one account is the attack an IP-only limit misses.
    state
        .auth_limiter
        .check_login(&state.db, &body.email)
        .await?;

    let Some(user) = local::authenticate(&state.db, &body.email, &body.password).await else {
        // One response for every failure: unknown email, wrong password, malformed input. Saying
        // "no such account" would confirm which addresses are registered.
        return Err(ApiError::Unauthorized("login failed"));
    };

    let session = local::issue_session(
        &state.db,
        &user.id,
        state.cfg.session_secret.expose(),
        &issuer(&state),
    )
    .await?;

    Ok(Json(SessionResponse {
        token: session.token,
        expires_at: session.expires_at.to_rfc3339(),
        user,
    }))
}

pub async fn logout(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> ApiResult<axum::http::StatusCode> {
    require_local(&state)?;
    // Deliberately not requiring a *valid* session: logging out with an expired or already-revoked
    // token should succeed quietly rather than fail, and revoking it again is harmless.
    if let Some(token) = crate::auth::claims::token_from_headers(&headers) {
        local::revoke_session(
            &state.db,
            token,
            state.cfg.session_secret.expose(),
            &issuer(&state),
        )
        .await?;
    }
    Ok(axum::http::StatusCode::NO_CONTENT)
}

pub async fn me(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<serde_json::Value>> {
    let id = Uuid::parse_str(user.id())
        .map_err(|_| ApiError::Unauthorized("subject is not a local user id"))?;

    match local::find_user(&state.db, &id).await? {
        Some(u) => Ok(Json(json!({
            "id": u.id,
            "email": u.email,
            "created_at": u.created_at.to_rfc3339(),
        }))),
        // A signature over a user that no longer exists. The token verifies; the account does not.
        None => Err(ApiError::Unauthorized("user no longer exists")),
    }
}

pub async fn change_password(
    State(state): State<AppState>,
    user: AuthUser,
    Json(body): Json<ChangePassword>,
) -> ApiResult<axum::http::StatusCode> {
    require_local(&state)?;
    let id = Uuid::parse_str(user.id())
        .map_err(|_| ApiError::Unauthorized("subject is not a local user id"))?;

    local::change_password(&state.db, &id, &body.current_password, &body.new_password).await?;
    // Every session is now revoked, including this one: the caller must log in again with the new
    // password. That is the point — a password change has to end sessions an attacker may hold.
    Ok(axum::http::StatusCode::NO_CONTENT)
}
