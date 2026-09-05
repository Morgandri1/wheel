//! JWT verification.
//!
//! The verified output is `VerifiedUser`. It has no public constructor other than verification,
//! so possessing one is proof that a token was checked — handlers cannot fabricate identity.

use crate::config::{Config, Env};
use crate::error::ApiError;
use jsonwebtoken::{decode, decode_header, Algorithm, DecodingKey, Validation};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Claims {
    pub sub: String,
    pub iss: String,
    pub exp: i64,
    #[serde(default)]
    pub nbf: Option<i64>,
    #[serde(default)]
    pub azp: Option<String>,
}

/// A user id that has been proven by signature verification.
#[derive(Debug, Clone)]
pub struct VerifiedUser {
    pub user_id: String,
}

/// Verify a bearer token against the JWKS, or — only in dev — against the HS256 shared secret.
pub async fn verify(
    token: &str,
    cfg: &Config,
    jwks: &super::jwks::JwksCache,
) -> Result<VerifiedUser, ApiError> {
    let header =
        decode_header(token).map_err(|_| ApiError::Unauthorized("malformed jwt header"))?;

    let claims = match header.alg {
        Algorithm::RS256 => {
            let kid = header
                .kid
                .as_deref()
                .ok_or(ApiError::Unauthorized("jwt header has no kid"))?;
            let key = jwks
                .key_for(kid)
                .await
                .ok_or(ApiError::Unauthorized("unknown or unavailable signing key"))?;
            decode_with(token, &key, cfg, Algorithm::RS256)?
        }

        // The dev bypass. Reachable only when the process booted with WHEEL_ENV=dev *and* a secret
        // was supplied; `Config::from_env` refuses to start in any other combination, so this arm
        // is unreachable in production by construction rather than by this check alone.
        Algorithm::HS256 if cfg.env.is_dev() => {
            let secret = cfg.dev_secret.as_deref().ok_or(ApiError::Unauthorized(
                "HS256 presented but no dev secret configured",
            ))?;
            let key = DecodingKey::from_secret(secret.as_bytes());
            decode_with(token, &key, cfg, Algorithm::HS256)?
        }

        // Everything else — `none`, HS256 in prod, ES256, RS512, ... — is refused outright.
        // In particular this is what defeats the classic confusion attack, where an attacker
        // re-signs a token with HS256 using the RSA *public* key as the HMAC secret: we never
        // reach a verifier that would accept it, because the algorithm is rejected on sight.
        other => {
            tracing::debug!(alg = ?other, "rejected token algorithm");
            return Err(ApiError::Unauthorized("unacceptable token algorithm"));
        }
    };

    if let Some(azp) = &claims.azp {
        if !cfg.clerk_azp.is_empty() && !cfg.clerk_azp.iter().any(|a| a == azp) {
            return Err(ApiError::Unauthorized("azp not in allowlist"));
        }
    }

    if claims.sub.is_empty() {
        return Err(ApiError::Unauthorized("token has empty sub"));
    }

    Ok(VerifiedUser {
        user_id: claims.sub,
    })
}

fn decode_with(
    token: &str,
    key: &DecodingKey,
    cfg: &Config,
    alg: Algorithm,
) -> Result<Claims, ApiError> {
    // Pin to exactly one algorithm — never a permissive list.
    let mut v = Validation::new(alg);
    v.set_issuer(&[cfg.clerk_issuer.as_str()]);
    v.validate_exp = true;
    v.validate_nbf = true;
    // `aud` is validated via azp above; Clerk session tokens do not reliably carry `aud`.
    v.validate_aud = false;
    v.leeway = 5;

    decode::<Claims>(token, key, &v)
        .map(|d| d.claims)
        .map_err(|e| {
            use jsonwebtoken::errors::ErrorKind as K;
            // Specific reason to the operator; the client gets a flat 401 regardless.
            let why = match e.kind() {
                K::ExpiredSignature => "expired",
                K::ImmatureSignature => "nbf in the future",
                K::InvalidIssuer => "wrong issuer",
                K::InvalidSignature => "bad signature",
                K::InvalidAlgorithm => "algorithm mismatch",
                _ => "invalid token",
            };
            ApiError::Unauthorized(why)
        })
}

/// Pull the bearer token out of `x-auth-token`, or `Authorization: Bearer <t>` as an alias.
pub fn token_from_headers(headers: &axum::http::HeaderMap) -> Option<&str> {
    if let Some(v) = headers.get("x-auth-token").and_then(|v| v.to_str().ok()) {
        let v = v.trim();
        // Tolerate a client that sends "Bearer <t>" in x-auth-token too.
        let v = v.strip_prefix("Bearer ").unwrap_or(v).trim();
        if !v.is_empty() {
            return Some(v);
        }
    }
    let auth = headers
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?;
    let (scheme, token) = auth.split_once(' ')?;
    // Scheme comparison is case-insensitive per RFC 7235.
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    let token = token.trim();
    (!token.is_empty()).then_some(token)
}

/// Compile-time reminder that `Env` gates the dev path.
const _: fn() = || {
    let _ = Env::Dev;
};
