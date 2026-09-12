// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

//! External authentication: the deployer brings an identity system, Wheel verifies and maps it.
//!
//! See `docs/proposals/external-auth.md`. Two verifiers, chosen by configuration:
//!
//!   * **`jwks`** — a JWT signed by the deployer's issuer, verified against their published keys.
//!   * **`proxy_header`** — a reverse proxy has already authenticated the user and names them in a
//!     header. Wheel verifies *nothing* about the assertion, so the whole control is that the
//!     request came from the proxy. See [`verify_proxy`].
//!
//! # Why the algorithm is never taken from the token
//!
//! `jsonwebtoken` will happily verify with whichever algorithm you hand it, and the obvious way to
//! choose one is `header.alg`. That hands the choice of verifier to whoever wrote the header, which
//! is the attacker. Here the `kid` resolves to a [`KeyEntry`] that carries the algorithm the *key
//! set* declares, the header must agree with it, the result must be in the operator's allowlist,
//! and only then is a `Validation` built pinned to that single algorithm.
//!
//! # Why `aud` needs `required_spec_claims`
//!
//! Measured from `jsonwebtoken-9.3.1/src/validation.rs:306-329`, not assumed: with
//! `validate_aud = true` and an audience configured, a token whose `aud` claim is *absent* falls
//! through the final `_ => {}` arm and **passes**. Validation is only applied to an `aud` that is
//! present. So the claim is made mandatory explicitly, and there is a test that would go red if
//! this were ever relaxed.

use crate::config::{ExternalAuth, ExternalVerifier, Provision};
use crate::db::Db;
use crate::error::{ApiError, ApiResult};
use axum::http::HeaderMap;
use jsonwebtoken::{decode, decode_header, Validation};
use uuid::Uuid;

/// What an external provider asserted, after verification and before mapping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verified {
    pub subject: String,
    pub email: Option<String>,
}

/// Verify a JWT from the deployer's issuer.
pub async fn verify_token(
    token: &str,
    ext: &ExternalAuth,
    jwks: &super::jwks::JwksCache,
) -> Result<Verified, ApiError> {
    let ExternalVerifier::Jwks { algs, .. } = &ext.verifier else {
        return Err(ApiError::Unauthorized(
            "a bearer token was presented but this deployment authenticates by proxy header",
        ));
    };

    let header = decode_header(token).map_err(|_| {
        // `alg: none` lands here: there is no `Algorithm` variant for it, so the header does not
        // even parse. Incidental to the dependency, so it is pinned by a test rather than a note.
        ApiError::Unauthorized("malformed jwt header")
    })?;
    let kid = header
        .kid
        .as_deref()
        .ok_or(ApiError::Unauthorized("jwt header has no kid"))?;
    let entry = jwks
        .key_for(kid)
        .await
        .ok_or(ApiError::Unauthorized("unknown or unavailable signing key"))?;

    if header.alg != entry.alg {
        return Err(ApiError::Unauthorized(
            "token algorithm does not match the signing key's",
        ));
    }
    if !algs.contains(&entry.alg) {
        return Err(ApiError::Unauthorized(
            "signing key's algorithm is not in the configured allowlist",
        ));
    }

    let mut v = Validation::new(entry.alg);
    v.set_issuer(&[ext.issuer.as_str()]);
    v.set_audience(&ext.audiences);
    // The three that must be present, not merely valid-if-present. `aud` above all: see the module
    // note. `iss` for the same reason — an absent issuer would otherwise skip the pin.
    v.set_required_spec_claims(&["exp", "iss", "aud"]);
    v.validate_exp = true;
    v.validate_nbf = true;
    v.validate_aud = true;
    v.leeway = 5;

    let claims = decode::<serde_json::Value>(token, &entry.key, &v)
        .map(|d| d.claims)
        .map_err(|e| {
            use jsonwebtoken::errors::ErrorKind as K;
            // Specific reason to the operator; the client gets a flat 401 regardless.
            ApiError::Unauthorized(match e.kind() {
                K::ExpiredSignature => "expired",
                K::ImmatureSignature => "nbf in the future",
                K::InvalidIssuer => "wrong issuer",
                K::InvalidAudience => "wrong audience",
                K::InvalidSignature => "bad signature",
                K::InvalidAlgorithm => "algorithm mismatch",
                K::MissingRequiredClaim(_) => "a required claim is absent",
                _ => "invalid token",
            })
        })?;

    checked_claims(&claims, ext).map_err(ApiError::Unauthorized)
}

/// The checks that are ours rather than the library's, over already-signature-verified claims.
///
/// Pure and separate so every rule below is testable against a literal claim set, with no signing,
/// no clock and no network. The signature is the library's job; these are the ones we would get
/// wrong.
pub(crate) fn checked_claims(
    claims: &serde_json::Value,
    ext: &ExternalAuth,
) -> Result<Verified, &'static str> {
    if ext.sole_audience && audience_count(claims) != 1 {
        // Any relying party named alongside us holds a token that works here. Off by default
        // because multi-audience access tokens are ordinary; on when the deployer does not extend
        // that trust.
        return Err("token names an audience other than this deployment");
    }

    if !ext.azp.is_empty() {
        let azp = claims.get("azp").and_then(|v| v.as_str()).unwrap_or("");
        if !ext.azp.iter().any(|a| a == azp) {
            return Err("azp not in allowlist");
        }
    }

    if let Some(cap) = ext.max_ttl_secs {
        // A cap without `iat` is not a cap: lifetime is exp minus issuance, and only the token
        // says when it was issued.
        let iat = claims
            .get("iat")
            .and_then(serde_json::Value::as_i64)
            .ok_or("a token lifetime cap is configured but the token has no iat")?;
        let exp = claims
            .get("exp")
            .and_then(serde_json::Value::as_i64)
            .ok_or("token has no numeric exp")?;
        if exp.saturating_sub(iat) > cap {
            return Err("token lifetime exceeds the configured maximum");
        }
    }

    let subject = claims
        .get(&ext.subject_claim)
        .and_then(|v| v.as_str())
        .ok_or("token has no usable subject claim")?;
    super::principal::validate(subject)
        .map_err(|_| "token subject is not a usable principal")?;

    Ok(Verified {
        subject: subject.to_string(),
        // Display only. Never a link key — see `principal_for`.
        email: claims
            .get("email")
            .and_then(|v| v.as_str())
            .map(str::to_string),
    })
}

/// How many audiences the token names. `0` for absent, which `checked_claims` treats as "not one".
fn audience_count(claims: &serde_json::Value) -> usize {
    match claims.get("aud") {
        Some(serde_json::Value::String(_)) => 1,
        Some(serde_json::Value::Array(a)) => a.len(),
        _ => 0,
    }
}

/// Take the caller's identity from a header a reverse proxy set.
///
/// **Nothing here is verified.** The proxy is the verifier, so the entire control is that this
/// request reached us *from* the proxy: `trusted_peer` is computed from the TCP peer address by
/// `http::client_ip`, is a server-side request extension, and cannot be influenced by a client. If
/// it is false — including when the middleware that computes it is not installed at all — this
/// fails closed.
pub fn verify_proxy(
    headers: &HeaderMap,
    trusted_peer: bool,
    ext: &ExternalAuth,
) -> Result<Verified, ApiError> {
    let ExternalVerifier::ProxyHeader {
        subject_header,
        email_header,
    } = &ext.verifier
    else {
        return Err(ApiError::Unauthorized(
            "this deployment authenticates with a token, not a proxy header",
        ));
    };

    if !trusted_peer {
        return Err(ApiError::Unauthorized(
            "proxy-header auth from a peer that is not a trusted proxy",
        ));
    }

    let subject = headers
        .get(subject_header.as_str())
        .and_then(|v| v.to_str().ok())
        .ok_or(ApiError::Unauthorized("no subject header from the proxy"))?;
    super::principal::validate(subject)
        .map_err(|_| ApiError::Unauthorized("proxy subject is not a usable principal"))?;

    Ok(Verified {
        subject: subject.to_string(),
        email: email_header
            .as_deref()
            .and_then(|h| headers.get(h))
            .and_then(|v| v.to_str().ok())
            .map(str::to_string),
    })
}

/// Map a verified external subject to the Wheel principal that membership and attribution key off.
///
/// The returned string is a Wheel `users.id`, never the foreign subject. That is the whole point:
/// `projects.owner_id`, `project_members.user_id` and `on_behalf_of` all name something Wheel
/// minted and no external system can change.
///
/// The link key is `(issuer, subject)` and nothing else. **Never email** — an IdP that lets a user
/// set an unverified address would otherwise be a one-step takeover of any local account whose
/// address an attacker can guess.
pub async fn principal_for(db: &Db, ext: &ExternalAuth, v: &Verified) -> ApiResult<String> {
    if let Some(existing) = lookup(db, &ext.issuer, &v.subject).await? {
        return match existing.disabled_at {
            // An operator disabled this identity. Verification succeeds — the IdP still vouches
            // for them — and access does not, which is the only revocation lever Wheel has over a
            // provider that has no back-channel logout.
            Some(_) => Err(ApiError::Unauthorized("external identity is disabled")),
            None => {
                touch(db, &existing.id).await?;
                Ok(existing.user_id.to_string())
            }
        };
    }

    if ext.provision == Provision::Linked {
        return Err(ApiError::Unauthorized(
            "external subject is not linked to an account and provisioning is `linked`",
        ));
    }

    let user = super::local::create_external_user(db).await?;
    link(db, ext, v, user.id, None).await?;
    Ok(user.id.to_string())
}

/// A row of `external_identities`.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ExternalIdentity {
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

const COLUMNS: &str =
    "id, provider, issuer, subject, user_id, email, created_at, last_seen_at, disabled_at";

pub async fn lookup(db: &Db, issuer: &str, subject: &str) -> ApiResult<Option<ExternalIdentity>> {
    Ok(crate::db_fetch_optional!(
        db,
        &format!("SELECT {COLUMNS} FROM external_identities WHERE issuer = $1 AND subject = $2"),
        issuer,
        subject
    )?)
}

pub async fn list_all(db: &Db) -> ApiResult<Vec<ExternalIdentity>> {
    Ok(crate::db_fetch_all!(
        db,
        &format!("SELECT {COLUMNS} FROM external_identities ORDER BY created_at, id")
    )?)
}

/// Record an external identity against a Wheel account.
pub async fn link(
    db: &Db,
    ext: &ExternalAuth,
    v: &Verified,
    user_id: Uuid,
    _linked_by: Option<&str>,
) -> ApiResult<ExternalIdentity> {
    let id = Uuid::new_v4();
    crate::db_execute!(
        db,
        "INSERT INTO external_identities (id, provider, issuer, subject, user_id, email) \
         VALUES ($1, $2, $3, $4, $5, $6)",
        id,
        &ext.provider,
        &ext.issuer,
        &v.subject,
        user_id,
        v.email.as_deref()
    )
    .map_err(|e| {
        if crate::db::is_unique_violation(&e) {
            ApiError::Conflict("that external subject is already linked to an account".into())
        } else {
            ApiError::from(e)
        }
    })?;
    lookup(db, &ext.issuer, &v.subject)
        .await?
        .ok_or_else(|| ApiError::Internal(anyhow::anyhow!("linked identity vanished")))
}

/// Disable an identity. Soft, so the link is still visible to an operator afterwards, and so
/// re-enabling is a decision rather than a re-provision under a new Wheel user.
pub async fn disable(db: &Db, id: &Uuid) -> ApiResult<bool> {
    let now = db.pick("now()", "strftime('%Y-%m-%dT%H:%M:%fZ', 'now')");
    let sql = format!(
        "UPDATE external_identities SET disabled_at = COALESCE(disabled_at, {now}) WHERE id = $1"
    );
    Ok(crate::db_execute!(db, &sql, id)? > 0)
}

async fn touch(db: &Db, id: &Uuid) -> ApiResult<()> {
    let now = db.pick("now()", "strftime('%Y-%m-%dT%H:%M:%fZ', 'now')");
    let sql = format!("UPDATE external_identities SET last_seen_at = {now} WHERE id = $1");
    crate::db_execute!(db, &sql, id)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ExternalAuth;

    fn ext() -> ExternalAuth {
        ExternalAuth::for_test()
    }

    fn claims(v: serde_json::Value) -> serde_json::Value {
        v
    }

    #[test]
    fn a_well_formed_claim_set_yields_the_subject_and_email() {
        let got = checked_claims(
            &claims(serde_json::json!({
                "sub": "alice", "aud": "wheel-test", "exp": 2_000_000_000i64,
                "email": "alice@example.com"
            })),
            &ext(),
        )
        .unwrap();
        assert_eq!(got.subject, "alice");
        assert_eq!(got.email.as_deref(), Some("alice@example.com"));
    }

    #[test]
    fn a_subject_that_is_not_a_principal_is_refused() {
        let e = checked_claims(
            &claims(serde_json::json!({ "sub": "a\nb", "aud": "wheel-test", "exp": 1 })),
            &ext(),
        )
        .unwrap_err();
        assert!(e.contains("principal"), "{e}");
    }

    #[test]
    fn a_missing_subject_claim_is_refused_rather_than_defaulted() {
        assert!(checked_claims(
            &claims(serde_json::json!({ "aud": "wheel-test", "exp": 1 })),
            &ext()
        )
        .is_err());
        // A non-string subject must not be coerced: `sub: 7` is not principal "7".
        assert!(checked_claims(
            &claims(serde_json::json!({ "sub": 7, "aud": "wheel-test", "exp": 1 })),
            &ext()
        )
        .is_err());
    }

    #[test]
    fn a_configured_subject_claim_is_read_instead_of_sub() {
        let mut e = ext();
        e.subject_claim = "oid".into();
        let got = checked_claims(
            &claims(serde_json::json!({ "sub": "mutable", "oid": "stable-1", "aud": "wheel-test", "exp": 1 })),
            &e,
        )
        .unwrap();
        assert_eq!(got.subject, "stable-1");
    }

    #[test]
    fn sole_audience_refuses_a_token_shared_with_another_relying_party() {
        let mut e = ext();
        e.sole_audience = true;
        assert!(checked_claims(
            &claims(serde_json::json!({
                "sub": "alice", "aud": ["wheel-test", "someone-else"], "exp": 1
            })),
            &e
        )
        .is_err());
        assert!(checked_claims(
            &claims(serde_json::json!({ "sub": "alice", "aud": ["wheel-test"], "exp": 1 })),
            &e
        )
        .is_ok());
        assert!(checked_claims(
            &claims(serde_json::json!({ "sub": "alice", "aud": "wheel-test", "exp": 1 })),
            &e
        )
        .is_ok());
    }

    #[test]
    fn multiple_audiences_are_accepted_when_sole_audience_is_off() {
        assert!(checked_claims(
            &claims(serde_json::json!({
                "sub": "alice", "aud": ["wheel-test", "someone-else"], "exp": 1
            })),
            &ext()
        )
        .is_ok());
    }

    #[test]
    fn the_lifetime_cap_needs_iat_and_refuses_a_long_token() {
        let mut e = ext();
        e.max_ttl_secs = Some(300);
        // No iat: a cap that cannot be computed must refuse, not pass.
        let no_iat =
            checked_claims(&claims(serde_json::json!({ "sub": "a", "aud": "wheel-test", "exp": 1_000 })), &e)
                .unwrap_err();
        assert!(no_iat.contains("iat"), "{no_iat}");

        assert!(checked_claims(
            &claims(serde_json::json!({ "sub": "a", "aud": "wheel-test", "iat": 1_000, "exp": 1_301 })),
            &e
        )
        .is_err());
        assert!(checked_claims(
            &claims(serde_json::json!({ "sub": "a", "aud": "wheel-test", "iat": 1_000, "exp": 1_300 })),
            &e
        )
        .is_ok());
    }

    #[test]
    fn azp_is_checked_only_when_configured() {
        let mut e = ext();
        let c = claims(serde_json::json!({ "sub": "a", "aud": "wheel-test", "exp": 1, "azp": "app-1" }));
        assert!(checked_claims(&c, &e).is_ok(), "empty allowlist checks nothing");
        e.azp = vec!["app-2".into()];
        assert!(checked_claims(&c, &e).is_err());
        e.azp = vec!["app-1".into(), "app-2".into()];
        assert!(checked_claims(&c, &e).is_ok());
        // A token with no azp at all cannot satisfy a non-empty allowlist.
        assert!(checked_claims(
            &claims(serde_json::json!({ "sub": "a", "aud": "wheel-test", "exp": 1 })),
            &e
        )
        .is_err());
    }

    #[test]
    fn proxy_auth_fails_closed_without_a_trusted_peer() {
        let e = ExternalAuth::for_test_proxy();
        let mut h = HeaderMap::new();
        h.insert("x-forwarded-user", "alice".parse().unwrap());

        assert!(
            verify_proxy(&h, false, &e).is_err(),
            "an untrusted peer must never be believed"
        );
        assert_eq!(verify_proxy(&h, true, &e).unwrap().subject, "alice");
    }

    #[test]
    fn proxy_auth_refuses_a_subject_that_is_not_a_principal() {
        let e = ExternalAuth::for_test_proxy();
        let mut h = HeaderMap::new();
        // A header value cannot hold a raw newline, so the reachable hostile shape is a quote —
        // which would otherwise close an envelope attribute.
        h.insert("x-forwarded-user", "alice\" tier=\"admin".parse().unwrap());
        assert!(verify_proxy(&h, true, &e).is_err());
    }

    #[test]
    fn proxy_auth_refuses_a_missing_subject_header() {
        let e = ExternalAuth::for_test_proxy();
        assert!(verify_proxy(&HeaderMap::new(), true, &e).is_err());
    }

    /// The two verifiers must not stand in for each other: a deployment configured for one
    /// refuses the other's credential rather than falling through to a weaker check.
    #[test]
    fn a_verifier_refuses_the_other_verifiers_credential() {
        let mut h = HeaderMap::new();
        h.insert("x-forwarded-user", "alice".parse().unwrap());
        assert!(
            verify_proxy(&h, true, &ext()).is_err(),
            "jwks mode must not accept a proxy header"
        );
    }
}
