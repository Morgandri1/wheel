//! Header hygiene for both proxy paths.
//!
//! A proxy that forwards headers naively is a confused deputy. Two distinct jobs here:
//!
//!  1. **Hop-by-hop headers** (RFC 9110 §7.6.1) are meaningful only on a single connection and must
//!     never be relayed, including any header the client *names* in `Connection` — otherwise a
//!     client can nominate arbitrary headers for stripping, or smuggle framing directives.
//!  2. **Credential scrubbing.** The client's own `x-auth-token` must not reach the engine; the
//!     engine authenticates the *API*, not the user, and forwarding user credentials to a
//!     downstream service is how token-replay bugs are born.

use axum::http::{HeaderMap, HeaderName};

/// Headers that never cross a proxy hop.
const HOP_BY_HOP: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

/// Credentials and routing headers that belong to *our* boundary and must not be relayed upstream.
const CLIENT_ONLY: &[&str] = &[
    "x-auth-token",
    "authorization",
    "x-project-id",
    "host",
    "content-length",
];

/// Build the header set to send upstream.
///
/// `extra_forbidden_prefixes` lets the ingress path additionally drop every `x-wheel-*` header, so
/// a public caller cannot forge the trust markers we ourselves add.
pub fn sanitize_for_upstream(inbound: &HeaderMap, extra_forbidden_prefixes: &[&str]) -> HeaderMap {
    // Headers the client nominated via `Connection: foo, bar` are hop-by-hop for this exchange.
    let mut nominated: Vec<String> = Vec::new();
    for v in inbound.get_all("connection").iter() {
        if let Ok(s) = v.to_str() {
            nominated.extend(
                s.split(',')
                    .map(|t| t.trim().to_ascii_lowercase())
                    .filter(|t| !t.is_empty()),
            );
        }
    }

    let mut out = HeaderMap::new();
    for (name, value) in inbound.iter() {
        let n = name.as_str().to_ascii_lowercase();
        if HOP_BY_HOP.contains(&n.as_str())
            || CLIENT_ONLY.contains(&n.as_str())
            || nominated.iter().any(|x| x == &n)
            || extra_forbidden_prefixes.iter().any(|p| n.starts_with(p))
        {
            continue;
        }
        out.append(name.clone(), value.clone());
    }
    out
}

/// Filter a response coming back from upstream before it reaches the client.
pub fn sanitize_from_upstream(inbound: &HeaderMap) -> HeaderMap {
    let mut out = HeaderMap::new();
    for (name, value) in inbound.iter() {
        let n = name.as_str().to_ascii_lowercase();
        // `content-length` is dropped because we may re-frame the body when streaming; hyper
        // recomputes framing for the outbound response.
        if HOP_BY_HOP.contains(&n.as_str()) || n == "content-length" {
            continue;
        }
        out.append(name.clone(), value.clone());
    }
    out
}

pub fn header_name(s: &str) -> HeaderName {
    HeaderName::from_bytes(s.as_bytes()).expect("static header name is valid")
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn hm(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut m = HeaderMap::new();
        for (k, v) in pairs {
            m.append(header_name(k), HeaderValue::from_str(v).unwrap());
        }
        m
    }

    #[test]
    fn strips_user_credentials() {
        let out = sanitize_for_upstream(
            &hm(&[
                ("x-auth-token", "clerk-jwt"),
                ("authorization", "Bearer clerk-jwt"),
                ("x-project-id", "abc"),
                ("content-type", "application/json"),
            ]),
            &[],
        );
        assert!(out.get("x-auth-token").is_none());
        assert!(
            out.get("authorization").is_none(),
            "user token must not reach the engine"
        );
        assert!(out.get("x-project-id").is_none());
        assert_eq!(out.get("content-type").unwrap(), "application/json");
    }

    #[test]
    fn strips_hop_by_hop_and_nominated() {
        let out = sanitize_for_upstream(
            &hm(&[
                ("connection", "x-secret-thing, keep-alive"),
                ("x-secret-thing", "leak"),
                ("transfer-encoding", "chunked"),
                ("upgrade", "websocket"),
                ("x-keep", "yes"),
            ]),
            &[],
        );
        assert!(
            out.get("x-secret-thing").is_none(),
            "Connection-nominated header must be dropped"
        );
        assert!(out.get("transfer-encoding").is_none());
        assert!(out.get("upgrade").is_none());
        assert_eq!(out.get("x-keep").unwrap(), "yes");
    }

    #[test]
    fn ingress_drops_forged_wheel_headers() {
        let out = sanitize_for_upstream(
            &hm(&[
                ("x-wheel-ingress", "1"),
                ("x-wheel-anything", "forged"),
                ("accept", "*/*"),
            ]),
            &["x-wheel-"],
        );
        assert!(
            out.get("x-wheel-ingress").is_none(),
            "public caller forged a trust marker"
        );
        assert!(out.get("x-wheel-anything").is_none());
        assert_eq!(out.get("accept").unwrap(), "*/*");
    }

    #[test]
    fn case_insensitive() {
        let out = sanitize_for_upstream(&hm(&[("X-Auth-Token", "t"), ("AUTHORIZATION", "b")]), &[]);
        assert!(out.is_empty(), "header matching must be case-insensitive");
    }
}
