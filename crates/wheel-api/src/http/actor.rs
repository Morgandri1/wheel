// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

//! The trust markers the API adds when it forwards a request to a project's engine.
//!
//! # The rule, and why it is one function
//!
//! **Strip the whole `x-wheel-` namespace, then set our own.** In that order, always. The ingress
//! route has done this since it was written (`routes/ingress.rs`); the authenticated engine proxy
//! did not, which is the defect this module exists to close — see the finding in
//! `redteam/findings/052-authenticated-proxy-does-not-strip-x-wheel-namespace.md`.
//! `wheel-host` relays anything it is given, so a header the API fails to strip reaches the
//! engine looking exactly like one the API set.
//!
//! There are three places the API reaches an engine — the HTTP proxy, the WebSocket bridge, and
//! `HttpBoardClient` — and they build their outbound headers in three different ways. So the
//! strip-and-set is here, as one function each of them calls, rather than three times at three call
//! sites where two of them would eventually be a version behind.

use crate::auth::{AuthUser, Tier};
use crate::config::{Config, ExternalVerifier};
use axum::http::{HeaderMap, HeaderValue};

/// The namespace only the API may write. Everything under it is stripped from a client's request
/// before anything of ours is added.
pub const WHEEL_PREFIX: &str = "x-wheel-";

/// The verified principal on whose behalf this request is being made.
pub const ACTOR_ID: &str = "x-wheel-actor-id";
/// Their tier in this project: `admin`, `prompter` or `guest`.
pub const ACTOR_TIER: &str = "x-wheel-actor-tier";
/// How they proved who they are: `session`, `api_token` or `ws_ticket`.
pub const ACTOR_CREDENTIAL: &str = "x-wheel-actor-credential";

/// Add the actor markers to a header map that has already had `x-wheel-*` stripped.
///
/// The values cannot carry an injection: a principal's charset excludes control characters,
/// whitespace and quotes (`auth::principal`), and the tier and credential are `&'static str` from
/// closed enums. So `HeaderValue::from_str` cannot fail for any of the three — but it is still
/// handled rather than unwrapped, because an auth path is the wrong place to learn that an
/// invariant moved.
pub fn set_actor(headers: &mut HeaderMap, user: &AuthUser, tier: Tier) {
    let pairs = [
        (ACTOR_ID, user.id()),
        (ACTOR_TIER, tier.as_str()),
        (ACTOR_CREDENTIAL, user.credential().as_str()),
    ];
    for (name, value) in pairs {
        match HeaderValue::from_str(value) {
            Ok(v) => {
                headers.insert(super::hop::header_name(name), v);
            }
            Err(_) => {
                tracing::error!(header = name, "actor value was not a legal header value");
            }
        }
    }
}

/// The header names that carry this deployment's external credential, and so must never be relayed.
///
/// Named for what the list *is* rather than for who set it. It was `proxy_asserted_headers` and
/// returned an empty list under the `jwks` verifier, which read as correct — a token header is not
/// "proxy-asserted" — and the deployer's `WHEEL_EXTERNAL_TOKEN_HEADER` was relayed into the engine
/// as a result (ADVERSARY 064). The question here is only ever "is it a credential".
///
/// Two kinds, both credentials:
///   * under the `proxy_header` verifier, the subject and email headers the authenticating proxy
///     sets — they ARE the identity;
///   * under any external verifier, the header the deployer told Wheel to read the token from
///     (`WHEEL_EXTERNAL_TOKEN_HEADER`, e.g. Cloudflare Access's `cf-access-jwt-assertion`) — a
///     signed identity token, which is `x-auth-token` by another name. Without it here an agent
///     could read, and replay against this API, the token of the person it is talking to, and
///     public ingress would put it in a stored, guest-readable message body.
///
/// They cannot live in `hop::CLIENT_ONLY`, which is a `const`, because the deployer chooses their
/// names — so they are resolved from configuration here, at the one function every outbound engine
/// request goes through, rather than at each of the call sites that would otherwise have to
/// remember.
pub fn credential_headers(cfg: &Config) -> Vec<&str> {
    let Some(ext) = cfg.external.as_ref() else {
        return Vec::new();
    };
    let mut names = Vec::new();
    if let ExternalVerifier::ProxyHeader {
        subject_header,
        email_header,
    } = &ext.verifier
    {
        names.push(subject_header.as_str());
        names.extend(email_header.as_deref());
    }
    names.extend(ext.token_header.as_deref());
    names
}

/// The header set to send upstream: the client's, minus hop-by-hop, minus their credentials —
/// including a proxy-asserted identity — minus the entire `x-wheel-` namespace, plus ours.
///
/// A caller that forges `x-wheel-actor-tier: admin` therefore has it removed and then *replaced*
/// with their real tier — not merely ignored. The distinction matters: "ignored" would still leave
/// their value in the map beside ours, and `HeaderMap` can hold two values for one name.
pub fn sanitized_with_actor(
    inbound: &HeaderMap,
    cfg: &Config,
    user: &AuthUser,
    tier: Tier,
) -> HeaderMap {
    let mut headers =
        super::hop::sanitize_for_upstream(inbound, &[WHEEL_PREFIX], &credential_headers(cfg));
    set_actor(&mut headers, user, tier);
    headers
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The never-relay list has to come from configuration, and a deployment with no proxy
    /// verifier must not acquire one out of nowhere.
    #[test]
    fn the_proxy_assertion_names_come_from_configuration_or_nowhere() {
        let mut cfg = crate::config::Config::for_test();
        assert!(
            credential_headers(&cfg).is_empty(),
            "no external auth means nothing extra to strip"
        );

        let mut ext = crate::config::ExternalAuth::for_test();
        cfg.external = Some(ext.clone());
        assert!(
            credential_headers(&cfg).is_empty(),
            "the jwks verifier asserts nothing through a header"
        );

        ext.verifier = crate::config::ExternalVerifier::ProxyHeader {
            subject_header: "x-forwarded-user".into(),
            email_header: Some("x-forwarded-email".into()),
        };
        cfg.external = Some(ext.clone());
        assert_eq!(
            credential_headers(&cfg),
            vec!["x-forwarded-user", "x-forwarded-email"]
        );

        // The deployer's token header is a credential under EITHER verifier, and is listed in
        // addition to the proxy names rather than instead of them.
        ext.token_header = Some("cf-access-jwt-assertion".into());
        cfg.external = Some(ext.clone());
        assert_eq!(
            credential_headers(&cfg),
            vec![
                "x-forwarded-user",
                "x-forwarded-email",
                "cf-access-jwt-assertion"
            ]
        );
        ext.verifier = crate::config::ExternalVerifier::Jwks {
            url: "https://idp.example/jwks".into(),
            algs: vec![jsonwebtoken::Algorithm::EdDSA],
        };
        cfg.external = Some(ext.clone());
        assert_eq!(
            credential_headers(&cfg),
            vec!["cf-access-jwt-assertion"],
            "the jwks verifier's token header must be stripped too"
        );
        ext.token_header = None;
        ext.verifier = crate::config::ExternalVerifier::ProxyHeader {
            subject_header: "x-forwarded-user".into(),
            email_header: Some("x-forwarded-email".into()),
        };
        // The email header is optional, and its absence must not drop the subject with it.
        ext.verifier = crate::config::ExternalVerifier::ProxyHeader {
            subject_header: "x-forwarded-user".into(),
            email_header: None,
        };
        cfg.external = Some(ext);
        assert_eq!(credential_headers(&cfg), vec!["x-forwarded-user"]);
    }

    #[test]
    fn the_names_live_under_the_stripped_namespace() {
        // If one of these ever stopped starting with the prefix, the strip would no longer remove
        // a client's forgery of it and the replacement above would sit beside theirs.
        for n in [ACTOR_ID, ACTOR_TIER, ACTOR_CREDENTIAL] {
            assert!(
                n.starts_with(WHEEL_PREFIX),
                "{n} must be inside the namespace"
            );
        }
    }
}
