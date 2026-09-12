// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

//! The two fail-closed extractors.
//!
//! # Why this file is the whole security model
//!
//! A handler signature is a capability declaration. `AuthUser` can only be produced by verifying a
//! credential; `ProjectScope` can only be produced by verifying one *and* proving membership.
//! Neither has a public constructor. So a handler that wants to touch a project has exactly one way
//! to name one — take a `ProjectScope` — and by the time its body runs, the checks have happened.
//!
//! The failure mode this design eliminates is the common one: a handler that takes a raw
//! `Path<Uuid>` and forgets the access query. Here, that handler cannot be written, because there
//! is no function anywhere that turns a `Uuid` into a `Project` without the membership predicate.
//!
//! # What changed for multiplayer
//!
//! The predicate used to be `owner_id == jwt.sub`. It is now "is a member, and at which tier"
//! ([`load_member`]). The property above is unchanged and extended: `ProjectScope` now also carries
//! the caller's [`Tier`], so a handler cannot act without the tier having been resolved either.

use crate::error::ApiError;
use crate::models::{Project, ProjectRow};
use crate::state::AppState;
use axum::extract::{FromRequestParts, RawPathParams};
use axum::http::request::Parts;
use uuid::Uuid;

/// How a request proved who it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Credential {
    /// A login: a local session JWT (by session id), or the identity provider's token (none).
    Session(Option<Uuid>),
    /// A long-lived `wht_` API token, by id.
    ApiToken(Uuid),
    /// The deployer's own identity system (`AUTH_MODE=external`).
    External,
    /// A redeemed single-use WebSocket ticket. Only ever produced on the events route.
    WsTicket,
}

impl Credential {
    /// The wire name, as the `x-wheel-actor-credential` header carries it.
    pub fn as_str(self) -> &'static str {
        match self {
            Credential::Session(_) => "session",
            Credential::ApiToken(_) => "api_token",
            Credential::External => "external",
            Credential::WsTicket => "ws_ticket",
        }
    }
}

/// What a member may do. Operator ruling, 2026-09-11: exactly three, ordered.
///
/// `Ord` is derived from declaration order, so `actual >= required` is the whole check and there is
/// no table of pairwise comparisons to get wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    /// View only. Sees the board, transcripts and logs; sends nothing.
    Guest,
    /// Manage context, and prompt agents.
    Prompter,
    /// Everything: board structure, members and invites, vault, project lifecycle, settings.
    Admin,
}

impl Tier {
    pub fn as_str(self) -> &'static str {
        match self {
            Tier::Guest => "guest",
            Tier::Prompter => "prompter",
            Tier::Admin => "admin",
        }
    }

    /// Parse a stored role. Unknown text is **not** a tier — a row whose role we cannot read is
    /// refused rather than defaulted, because every default here is either uselessly strict or
    /// silently permissive.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "guest" => Some(Tier::Guest),
            "prompter" => Some(Tier::Prompter),
            "admin" => Some(Tier::Admin),
            _ => None,
        }
    }

    /// The tiers that may be *granted* to a member. Every one of them, including admin: the ruling
    /// puts member management in the admin tier, which implies an admin may make another.
    pub fn grantable() -> [Tier; 3] {
        [Tier::Guest, Tier::Prompter, Tier::Admin]
    }
}

/// Proof that the request carried a valid credential. Fields are private to this module's
/// constructor path — the only way to obtain one is extraction.
#[derive(Debug, Clone)]
pub struct AuthUser {
    user_id: String,
    credential: Credential,
}

impl AuthUser {
    pub fn id(&self) -> &str {
        &self.user_id
    }

    pub fn credential(&self) -> Credential {
        self.credential
    }

    /// Build one from an identity this module itself resolved. `pub(crate)` and deliberately
    /// awkward to reach: the events route redeems a ws-ticket, which proves an identity by a
    /// different route than a header, and it still has to end at the same type.
    pub(crate) fn from_redeemed_ticket(user_id: String) -> Self {
        AuthUser {
            user_id,
            credential: Credential::WsTicket,
        }
    }
}

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        let presented = token_from_parts(parts, state);

        // API tokens first, and before the proxy-header branch below.
        //
        // A `wht_` token is *this API's own* credential, whichever provider signs sessions, so a
        // desktop client authenticates the same way against a laptop and against the cloud. It has
        // to keep working under every mode — including proxy-header auth, where there is otherwise
        // no bearer token at all and an earlier version of this function returned before ever
        // looking. That made `wheeld token` useless on a proxy-authenticated deployment: the one
        // credential an operator can use from a script.
        if let Some(token) = presented.as_deref() {
            if crate::auth::api_token::is_api_token(token) {
                let v = crate::auth::api_token::verify(&state.db, token).await?;
                return Ok(AuthUser {
                    user_id: v.user_id,
                    credential: Credential::ApiToken(v.token_id),
                });
            }
        }

        // Proxy-header auth carries no token: the proxy authenticated the user and names them in a
        // header. Nothing about that assertion is verified, so the trusted-peer check is the whole
        // control — and its absence, including when the middleware is not installed, fails closed.
        if let Some(ext) = &state.cfg.external {
            if matches!(
                ext.verifier,
                crate::config::ExternalVerifier::ProxyHeader { .. }
            ) {
                let trusted = parts
                    .extensions
                    .get::<crate::http::client_ip::TrustedPeer>()
                    .is_some();
                let v = crate::auth::external::verify_proxy(&parts.headers, trusted, ext)?;
                let user_id = crate::auth::external::principal_for(&state.db, ext, &v).await?;
                return Ok(AuthUser {
                    user_id,
                    credential: Credential::External,
                });
            }
        }

        let token = presented.ok_or(ApiError::Unauthorized("no bearer token presented"))?;

        // The providers end here, at the same user id. Everything downstream — ProjectScope above
        // all — cannot tell which one ran, which is what makes swapping them configuration rather
        // than a rewrite. A token minted by a mode we are not in fails: local sessions are HS256
        // against our own secret, jwks tokens are RS256 against the provider's keys, and external
        // tokens are pinned to the deployer's issuer.
        let (user_id, credential) = match state.cfg.auth_mode {
            crate::config::AuthMode::Local => {
                let live = crate::auth::local::verify_session(
                    &state.db,
                    &token,
                    state.cfg.session_secret.expose(),
                    &state.cfg.public_base_url,
                )
                .await?;
                (live.user_id, Credential::Session(Some(live.session_id)))
            }
            crate::config::AuthMode::Jwks => (
                crate::auth::claims::verify(&token, &state.cfg, &state.jwks)
                    .await?
                    .user_id,
                Credential::Session(None),
            ),
            crate::config::AuthMode::External => {
                let ext = state.cfg.external.as_ref().ok_or_else(|| {
                    // Unreachable by construction: `Config::from_env` refuses to boot with
                    // AUTH_MODE=external and no block. Stated as an error rather than a panic
                    // because an auth path is the wrong place to be certain.
                    ApiError::Internal(anyhow::anyhow!("external auth mode with no configuration"))
                })?;
                let jwks = state.external_jwks.as_ref().ok_or_else(|| {
                    ApiError::Internal(anyhow::anyhow!("external auth mode with no key source"))
                })?;
                let v = crate::auth::external::verify_token(&token, ext, jwks).await?;
                (
                    crate::auth::external::principal_for(&state.db, ext, &v).await?,
                    Credential::External,
                )
            }
        };

        Ok(AuthUser {
            user_id,
            credential,
        })
    }
}

/// The credential, from wherever this deployment carries it.
///
/// Under external auth a deployer may name a different header — `cf-access-jwt-assertion` for
/// Cloudflare Access, which signs its assertion rather than merely asserting it. When they have,
/// that header is the *only* one read: falling back to `Authorization` would mean a caller could
/// choose which of two doors to knock on.
fn token_from_parts(parts: &Parts, state: &AppState) -> Option<String> {
    if let Some(name) = state
        .cfg
        .external
        .as_ref()
        .and_then(|e| e.token_header.as_deref())
    {
        return parts
            .headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(|v| v.trim().strip_prefix("Bearer ").unwrap_or(v.trim()))
            .filter(|v| !v.is_empty())
            .map(str::to_string);
    }
    crate::auth::claims::token_from_headers(&parts.headers).map(str::to_string)
}

/// Proof that the request carried a valid credential **and** that its subject is a member of the
/// project, at a known tier.
pub struct ProjectScope {
    pub user: AuthUser,
    pub project: Project,
    pub tier: Tier,
}

impl ProjectScope {
    /// Refuse unless the caller's tier reaches `needed`.
    ///
    /// The whole check is `self.tier >= needed`, which works because `Tier` is ordered. Every
    /// project-scoped handler calls this; the behavioural matrix in `tests/tiers.rs` is what makes
    /// that mechanical rather than remembered, by driving each route as a lower tier and requiring
    /// a 403.
    ///
    /// 403 rather than 404: the caller has already proved they are a member, so the project's
    /// existence is not a secret being kept from them, and an honest "not at your tier" is what
    /// lets a UI say why. Non-members never reach here — `load_member` answered 404 already.
    pub fn require(&self, needed: Tier) -> Result<(), ApiError> {
        if self.tier >= needed {
            return Ok(());
        }
        Err(ApiError::Forbidden("your role does not permit this"))
    }

    /// Build a scope from an identity proved by a redeemed ws-ticket.
    ///
    /// Membership is resolved **here, at redemption**, never at mint. Same lesson `api_token`
    /// records for minting chains: a ticket minted a moment before a revocation landed and redeemed
    /// a moment after must not open a socket, and only a check at use time can see that.
    pub(crate) async fn from_redeemed_ticket(
        state: &AppState,
        user_id: String,
        project_id: &Uuid,
    ) -> Result<Self, ApiError> {
        let user = AuthUser::from_redeemed_ticket(user_id);
        let (project, tier) = load_member(state, project_id, user.id()).await?;
        Ok(ProjectScope {
            user,
            project,
            tier,
        })
    }
}

impl FromRequestParts<AppState> for ProjectScope {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        // Order is mandated by ARCHITECTURE §5: verify the credential, then load, then assert
        // access. Authentication first means an anonymous caller can never reach the database.
        let user = AuthUser::from_request_parts(parts, state).await?;
        let project_id = project_id_from_request(parts, state).await?;
        let (project, tier) = load_member(state, &project_id, user.id()).await?;
        Ok(ProjectScope {
            user,
            project,
            tier,
        })
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

/// Row shape of [`load_member`]: a project plus the caller's role in it.
#[derive(sqlx::FromRow)]
struct MemberRow {
    #[sqlx(flatten)]
    project: ProjectRow,
    role: String,
}

/// The **only** function in the codebase that turns a project id into a `Project`.
///
/// Access is expressed as a predicate in the `WHERE` clause rather than as a comparison after the
/// fetch. That is deliberate, and it is the same reason the owner check had it: "row does not
/// exist", "you are not a member" and "your membership was revoked" become one code path returning
/// one `NotFound`, so they can never drift into an enumeration oracle where status or timing
/// distinguishes them.
///
/// The creator is resolved from `projects.owner_id` and is always [`Tier::Admin`]. That is not a
/// second source of truth for membership: being the creator only ever *adds* admin, never subtracts
/// anything, so the two clauses cannot contradict each other. The one state that would look like a
/// contradiction — a member row for the creator — is refused when it is written.
pub async fn load_member(
    state: &AppState,
    id: &Uuid,
    user_id: &str,
) -> Result<(Project, Tier), ApiError> {
    const SQL: &str = "SELECT p.id, p.owner_id, p.name, p.capabilities, p.status, \
                p.created_at, p.updated_at, \
                CASE WHEN p.owner_id = $2 THEN 'admin' ELSE m.role END AS role \
           FROM projects p \
           LEFT JOIN project_members m \
             ON m.project_id = p.id AND m.user_id = $2 AND m.revoked_at IS NULL \
          WHERE p.id = $1 AND (p.owner_id = $2 OR m.role IS NOT NULL)";

    let row: Option<MemberRow> = crate::db_fetch_optional!(&state.db, SQL, id, user_id)?;
    let row = row.ok_or(ApiError::NotFound)?;

    // A role string we cannot parse is not access. It can only arise from a row written outside
    // the API — a hand-edited database, or a future tier this build does not know — and guessing
    // which way to round it is exactly the decision that should fail closed.
    let tier = Tier::parse(&row.role).ok_or(ApiError::NotFound)?;
    Ok((Project::from(row.project), tier))
}

/// Load a project *without* an access check. Used only by the public ingress route, which is
/// unauthenticated by design. Kept `pub(crate)` and named to make its use obvious in review.
pub async fn load_unauthenticated_for_ingress(
    state: &AppState,
    id: &Uuid,
) -> Result<Project, ApiError> {
    let row: Option<ProjectRow> = crate::db_fetch_optional!(
        &state.db,
        "SELECT id, owner_id, name, capabilities, status, created_at, updated_at \
         FROM projects WHERE id = $1",
        id
    )?;
    row.map(Project::from).ok_or(ApiError::NotFound)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiers_order_from_least_to_most_capable() {
        assert!(Tier::Guest < Tier::Prompter);
        assert!(Tier::Prompter < Tier::Admin);
        // The whole policy check is `actual >= required`, so this ordering is load-bearing.
        assert!(Tier::Admin >= Tier::Guest);
        assert!(!(Tier::Guest >= Tier::Prompter));
    }

    #[test]
    fn an_unknown_role_is_not_a_tier() {
        assert_eq!(Tier::parse("admin"), Some(Tier::Admin));
        assert_eq!(Tier::parse("prompter"), Some(Tier::Prompter));
        assert_eq!(Tier::parse("guest"), Some(Tier::Guest));
        for unknown in ["owner", "editor", "viewer", "ADMIN", "", "root"] {
            assert_eq!(Tier::parse(unknown), None, "{unknown} must not parse");
        }
    }

    #[test]
    fn the_wire_name_round_trips() {
        for t in Tier::grantable() {
            assert_eq!(Tier::parse(t.as_str()), Some(t));
        }
    }

    #[test]
    fn credential_names_are_stable() {
        assert_eq!(Credential::Session(None).as_str(), "session");
        assert_eq!(Credential::ApiToken(Uuid::nil()).as_str(), "api_token");
        assert_eq!(Credential::External.as_str(), "external");
        assert_eq!(Credential::WsTicket.as_str(), "ws_ticket");
    }
}
