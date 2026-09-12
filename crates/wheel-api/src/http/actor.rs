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

/// The header set to send upstream: the client's, minus hop-by-hop, minus their credentials, minus
/// the entire `x-wheel-` namespace, plus ours.
///
/// A caller that forges `x-wheel-actor-tier: admin` therefore has it removed and then *replaced*
/// with their real tier — not merely ignored. The distinction matters: "ignored" would still leave
/// their value in the map beside ours, and `HeaderMap` can hold two values for one name.
pub fn sanitized_with_actor(inbound: &HeaderMap, user: &AuthUser, tier: Tier) -> HeaderMap {
    let mut headers = super::hop::sanitize_for_upstream(inbound, &[WHEEL_PREFIX]);
    set_actor(&mut headers, user, tier);
    headers
}

#[cfg(test)]
mod tests {
    use super::*;

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
