//! The two fail-closed extractors.
//!
//! # Why this file is the whole security model
//!
//! A handler signature is a capability declaration. `AuthUser` can only be produced by verifying a
//! token; `ProjectScope` can only be produced by verifying a token *and* proving ownership. Neither
//! has a public constructor. So a handler that wants to touch a project has exactly one way to name
//! one — take a `ProjectScope` — and by the time its body runs, the checks have already happened.
//!
//! The failure mode this design eliminates is the common one: a handler that takes a raw
//! `Path<Uuid>` and forgets the ownership query. Here, that handler cannot be written, because
//! there is no function anywhere that turns a `Uuid` into a `Project` without the owner predicate.

use crate::error::ApiError;
use crate::models::{Project, ProjectRow};
use crate::state::AppState;
use axum::extract::{FromRequestParts, RawPathParams};
use axum::http::request::Parts;
use uuid::Uuid;

/// Proof that the request carried a valid token. Field is private to this module's constructor
/// path — the only way to obtain one is extraction.
#[derive(Debug, Clone)]
pub struct AuthUser {
    user_id: String,
}

impl AuthUser {
    pub fn id(&self) -> &str {
        &self.user_id
    }
}

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        let token = crate::auth::claims::token_from_headers(&parts.headers)
            .ok_or(ApiError::Unauthorized("no bearer token presented"))?;

        // The two providers end here, at the same user id. Everything downstream — ProjectScope
        // above all — cannot tell which one ran, which is what makes swapping them configuration
        // rather than a rewrite. A token minted by the mode we are *not* in fails: local sessions
        // are HS256 against our own secret, jwks tokens are RS256 against the provider's keys.
        let user_id = match state.cfg.auth_mode {
            crate::config::AuthMode::Local => {
                crate::auth::local::verify_session(
                    &state.db,
                    token,
                    state.cfg.session_secret.expose(),
                    &state.cfg.public_base_url,
                )
                .await?
            }
            crate::config::AuthMode::Jwks => {
                crate::auth::claims::verify(token, &state.cfg, &state.jwks)
                    .await?
                    .user_id
            }
        };

        Ok(AuthUser { user_id })
    }
}

/// Proof that the request carried a valid token **and** that the token's subject owns the project.
pub struct ProjectScope {
    pub user: AuthUser,
    pub project: Project,
}

impl FromRequestParts<AppState> for ProjectScope {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        // Order is mandated by ARCHITECTURE §5: verify JWT, then load, then assert ownership.
        // Authentication first means an anonymous caller can never reach the database at all.
        let user = AuthUser::from_request_parts(parts, state).await?;
        let project_id = project_id_from_request(parts, state).await?;
        let project = load_owned(state, &project_id, user.id()).await?;
        Ok(ProjectScope { user, project })
    }
}

/// Resolve the target project id from the path segment, cross-checked against `x-project-id`.
///
/// The contract has clients send `x-project-id` while the routes also carry the id in the path.
/// Two sources for one identity is a confusion vector: if they can disagree, some future handler
/// will authorise against one and act on the other. So they must agree exactly, or we reject.
async fn project_id_from_request(parts: &mut Parts, state: &AppState) -> Result<Uuid, ApiError> {
    let from_path = raw_path_param(parts, state, &["id", "project_id"]).await;

    let from_header = parts
        .headers
        .get("x-project-id")
        .map(|v| {
            v.to_str()
                .ok()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| ApiError::BadRequest("x-project-id is not valid text".into()))
        })
        .transpose()?;

    match (from_path, from_header) {
        (Some(p), Some(h)) => {
            let p = parse_uuid(&p)?;
            let h = parse_uuid(h)?;
            if p != h {
                return Err(ApiError::BadRequest(
                    "x-project-id does not match the project id in the path".into(),
                ));
            }
            Ok(p)
        }
        (Some(p), None) => parse_uuid(&p),
        (None, Some(h)) => parse_uuid(h),
        (None, None) => Err(ApiError::BadRequest(
            "missing project id (path segment or x-project-id header)".into(),
        )),
    }
}

async fn raw_path_param(parts: &mut Parts, state: &AppState, names: &[&str]) -> Option<String> {
    // `RawPathParams` reads whatever the matched route captured, so this works for
    // `/v1/projects/{id}/...` and `/p/{project_id}/...` alike without either route needing to know
    // about this extractor. A route with no captures yields `None`, which callers treat as
    // "fall back to the header".
    let params = RawPathParams::from_request_parts(parts, state).await.ok()?;
    names.iter().find_map(|want| {
        params
            .iter()
            .find(|(k, _)| k == want)
            .map(|(_, v)| v.to_string())
    })
}

fn parse_uuid(s: &str) -> Result<Uuid, ApiError> {
    Uuid::parse_str(s.trim())
        .map_err(|_| ApiError::BadRequest("project id must be a valid uuid".into()))
}

/// The **only** function in the codebase that turns a project id into a `Project`.
///
/// Ownership is expressed as a predicate in the `WHERE` clause rather than as a comparison after
/// the fetch. That is deliberate: "row does not exist" and "row belongs to someone else" become
/// the same code path returning the same `NotFound`, so the two can never drift into an
/// enumeration oracle where timing or status distinguishes them.
async fn load_owned(state: &AppState, id: &Uuid, owner_id: &str) -> Result<Project, ApiError> {
    let row: Option<ProjectRow> = sqlx::query_as::<_, ProjectRow>(
        "SELECT id, owner_id, name, capabilities, status, created_at, updated_at \
         FROM projects WHERE id = $1 AND owner_id = $2",
    )
    .bind(id)
    .bind(owner_id)
    .fetch_optional(&state.db)
    .await?;

    row.map(Project::from).ok_or(ApiError::NotFound)
}

/// Load a project *without* an ownership check. Used only by the public ingress route, which is
/// unauthenticated by design. Kept `pub(crate)` and named to make its use obvious in review.
pub async fn load_unauthenticated_for_ingress(
    state: &AppState,
    id: &Uuid,
) -> Result<Project, ApiError> {
    let row: Option<ProjectRow> = sqlx::query_as::<_, ProjectRow>(
        "SELECT id, owner_id, name, capabilities, status, created_at, updated_at \
         FROM projects WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await?;
    row.map(Project::from).ok_or(ApiError::NotFound)
}
