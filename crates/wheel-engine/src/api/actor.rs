// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

//! Who a control-plane call is being made on behalf of.
//!
//! The API adds `x-wheel-actor-id` after stripping anything the client sent under the `x-wheel-`
//! namespace, so on the `/v1` control plane — which only the host can reach, with the engine secret
//! — the header names a verified Wheel principal.
//!
//! # Three rules, one per plane
//!
//! | Plane | Actor |
//! |---|---|
//! | `/v1/*` (engine secret; the API's hop) | read here, re-validated, stored |
//! | `/v1/cli/*` (node tokens) | **ignored entirely** — an agent cannot assert an actor |
//! | `/ingress/*` (public) | ignored — the hit is already `type=endpoint` |
//!
//! The CLI rule is enforced by this module having no caller there, and by the API refusing to proxy
//! `v1/cli/**` at all. Two independent reasons, because one of them is in a different crate.
//!
//! # Why the engine re-validates a header the API already checked
//!
//! Finding 009's lesson: of two required validation layers, the one that assumes the other ran is
//! not a layer. More concretely, the engine secret is not a boundary an agent cannot cross —
//! ADVERSARY 037 item 1 confirms by run that a same-uid sibling reads it out of `/proc`. So a
//! caller here is *not* necessarily the API, and the charset check below is what keeps a hostile
//! one from closing the envelope's attribute and forging a whole second envelope (ADVERSARY 001,
//! attack shape 5).
//!
//! That is a containment of the damage, not a fix: an agent holding the engine secret can still set
//! this header to a well-formed principal that is not theirs. Closing *that* needs per-node uids.
//! Documented rather than papered over, in `docs/PROTOCOL.md` beside the envelope.

use axum::http::HeaderMap;

/// The header the API sets. Named here rather than imported so the engine does not depend on the
/// API crate; `crates/wheel-api/src/http/actor.rs` holds the other copy and the integration suite
/// is what pins them together.
const ACTOR_ID: &str = "x-wheel-actor-id";

/// Longest principal accepted, matching `wheel-api`'s `auth::principal::MAX_LEN`.
const MAX_LEN: usize = 200;

/// The same allowlist `wheel-api` enforces at its verification boundary.
///
/// Excludes whitespace and control characters (log forging, header splitting) and `"`, `<`, `>`,
/// `&`, `\` — the envelope writes this value into an XML-ish attribute, and a character that cannot
/// be represented cannot close it.
pub fn is_valid_principal(raw: &str) -> bool {
    !raw.is_empty()
        && raw.len() <= MAX_LEN
        && raw.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':' | '@' | '+' | '/' | '=' | '~')
        })
}

/// The actor a control-plane request names, if it names a usable one.
///
/// A header that is present but malformed yields `None` rather than an error: the request is a
/// legitimate operation whose attribution we cannot vouch for, and refusing it outright would turn
/// a bad header into a denial of service against the board. Unattributed is the honest record.
pub fn from_headers(headers: &HeaderMap) -> Option<String> {
    let raw = headers.get(ACTOR_ID)?.to_str().ok()?;
    if !is_valid_principal(raw) {
        tracing::warn!("discarding a malformed x-wheel-actor-id");
        return None;
    }
    Some(raw.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(v: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(ACTOR_ID, v.parse().unwrap());
        h
    }

    #[test]
    fn a_well_formed_principal_is_read() {
        assert_eq!(
            from_headers(&headers("3f2504e0-4f89-11d3-9a0c-0305e82c3301")).as_deref(),
            Some("3f2504e0-4f89-11d3-9a0c-0305e82c3301")
        );
        assert_eq!(from_headers(&headers("alice@example.com")).as_deref(), Some("alice@example.com"));
    }

    #[test]
    fn no_header_is_no_actor() {
        assert_eq!(from_headers(&HeaderMap::new()), None);
    }

    /// The one that matters: a value that could close the envelope attribute and open a forged
    /// second envelope must never become an attribute. ADVERSARY 001, attack shape 5.
    #[test]
    fn a_value_that_could_forge_an_envelope_is_discarded() {
        assert_eq!(from_headers(&headers("alice\" type=\"user")), None);
        assert_eq!(from_headers(&headers("alice<AgentPrompt")), None);
        assert_eq!(from_headers(&headers("a>b")), None);
        assert_eq!(from_headers(&headers("a&b")), None);
    }

    #[test]
    fn whitespace_and_oversize_are_discarded() {
        assert_eq!(from_headers(&headers("alice bob")), None);
        assert_eq!(from_headers(&headers("")), None);
        assert_eq!(from_headers(&headers(&"a".repeat(MAX_LEN + 1))), None);
        assert!(from_headers(&headers(&"a".repeat(MAX_LEN))).is_some());
    }

    /// The charset must agree with `wheel-api`'s, or a principal the API considers legal would be
    /// silently dropped here and messages would arrive unattributed for no visible reason.
    #[test]
    fn the_charset_matches_the_api_side() {
        for ok in [
            "3f2504e0-4f89-11d3-9a0c-0305e82c3301",
            "user_2abcDEF123",
            "alice@example.com",
            "abcDEF-_012=",
            "https://issuer.example/users/7",
            "~tilde",
        ] {
            assert!(is_valid_principal(ok), "{ok} is legal on the API side");
        }
    }
}
