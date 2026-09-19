// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

//! The address a request really came from, as an axum middleware.
//!
//! The trust rules live in [`wheel_core::client_ip`], shared with the sandbox host. This is only
//! the HTTP plumbing: read the peer and `X-Forwarded-For`, record the vouched-for client.
//!
//! `X-Forwarded-Proto` is not read at all. The scheme the API advertises comes from
//! `PUBLIC_BASE_URL`, which is configuration rather than a claim a request can make.

use axum::extract::{ConnectInfo, Request, State};
use axum::http::HeaderMap;
use axum::middleware::Next;
use axum::response::Response;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

pub use wheel_core::client_ip::{TrustedProxies, ENV_TRUSTED_PROXIES};

/// The caller's address, as far as this API can vouch for it. Present on a request when the server
/// was built with connect info and [`resolve`] ran.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientIp(pub IpAddr);

/// The client behind `peer`, from the request's `X-Forwarded-For` headers. A header that is not
/// text is treated as absent: nothing beyond the peer can then be vouched for.
pub fn client_of(trusted: &TrustedProxies, peer: IpAddr, headers: &HeaderMap) -> IpAddr {
    let values: Option<Vec<&str>> = headers
        .get_all("x-forwarded-for")
        .iter()
        .map(|v| v.to_str().ok())
        .collect();
    trusted.client(peer, &values.unwrap_or_default())
}

/// Marker: this request's **TCP peer** is inside `WHEEL_TRUSTED_PROXIES`.
///
/// The peer, deliberately — not `X-Forwarded-For`, which a client writes itself. Proxy-header
/// authentication believes a header, so the only thing standing between that header and anyone on
/// the internet is that the connection came from the proxy.
///
/// It is a request *extension*, set by [`resolve`] on the server side, so no client can present
/// one. And its absence — including when this middleware is not installed at all — reads as "not
/// trusted", so a deployment that forgets the layer refuses every proxy-authenticated request
/// instead of accepting every forged one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrustedPeer;

/// Middleware: record the vouched-for client address on the request as [`ClientIp`].
pub async fn resolve(
    State(trusted): State<Arc<TrustedProxies>>,
    mut req: Request,
    next: Next,
) -> Response {
    if let Some(ConnectInfo(peer)) = req.extensions().get::<ConnectInfo<SocketAddr>>().copied() {
        let client = client_of(&trusted, peer.ip(), req.headers());
        req.extensions_mut().insert(ClientIp(client));
        // The PEER, not `client`: `client` is the address the forwarding chain claims, and
        // proxy-header auth must depend on who actually connected.
        if trusted.trusts_peer(peer.ip()) {
            req.extensions_mut().insert(TrustedPeer);
        }
    }
    next.run(req).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_header_that_is_not_text_vouches_for_nothing() {
        let t = TrustedProxies::parse("127.0.0.1").unwrap();
        let peer: IpAddr = "127.0.0.1".parse().unwrap();
        let mut binary = HeaderMap::new();
        binary.insert(
            "x-forwarded-for",
            axum::http::HeaderValue::from_bytes(b"\xff").unwrap(),
        );
        assert_eq!(client_of(&t, peer, &binary), peer);
    }

    #[test]
    fn repeated_headers_are_one_list() {
        let t = TrustedProxies::parse("127.0.0.1").unwrap();
        let mut h = HeaderMap::new();
        h.append("x-forwarded-for", "6.6.6.6".parse().unwrap());
        h.append("x-forwarded-for", "198.51.100.7".parse().unwrap());
        assert_eq!(
            client_of(&t, "127.0.0.1".parse().unwrap(), &h),
            "198.51.100.7".parse::<IpAddr>().unwrap()
        );
    }
    #[tokio::test]
    async fn the_middleware_records_the_client_when_the_peer_is_known() {
        use axum::routing::get;
        use tower::ServiceExt;
        let app = axum::Router::new()
            .route(
                "/",
                get(|c: Option<axum::Extension<ClientIp>>| async move {
                    c.map(|axum::Extension(ClientIp(ip))| ip.to_string())
                        .unwrap_or_default()
                }),
            )
            .layer(axum::middleware::from_fn_with_state(
                Arc::new(TrustedProxies::parse("127.0.0.1").unwrap()),
                resolve,
            ));
        let body = |req: axum::http::Request<axum::body::Body>| {
            let app = app.clone();
            async move {
                let res = app.oneshot(req).await.unwrap();
                String::from_utf8(
                    axum::body::to_bytes(res.into_body(), 1024)
                        .await
                        .unwrap()
                        .to_vec(),
                )
                .unwrap()
            }
        };

        let mut req = axum::http::Request::builder()
            .uri("/")
            .header("x-forwarded-for", "198.51.100.7")
            .body(axum::body::Body::empty())
            .unwrap();
        req.extensions_mut()
            .insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 5555))));
        assert_eq!(body(req).await, "198.51.100.7");

        let bare = axum::http::Request::builder()
            .uri("/")
            .body(axum::body::Body::empty())
            .unwrap();
        assert_eq!(body(bare).await, "", "no peer, no claim");
    }

    /// `resolve` sets `TrustedPeer` from the **peer**, not from anything a header claims — the
    /// distinction `TrustedPeer`'s own doc comment exists to draw.
    #[tokio::test]
    async fn the_middleware_marks_trusted_peer_from_the_peer_only() {
        use axum::routing::get;
        use tower::ServiceExt;
        let app = axum::Router::new()
            .route(
                "/",
                get(|m: Option<axum::Extension<TrustedPeer>>| async move {
                    if m.is_some() { "trusted" } else { "untrusted" }
                }),
            )
            .layer(axum::middleware::from_fn_with_state(
                Arc::new(TrustedProxies::parse("127.0.0.1/32").unwrap()),
                resolve,
            ));
        let req = |peer: [u8; 4]| {
            let mut r = axum::http::Request::builder()
                .uri("/")
                // An untrusted peer claiming to forward for the trusted address must not
                // borrow that trust: the marker follows the connection, never the header.
                .header("x-forwarded-for", "127.0.0.1")
                .body(axum::body::Body::empty())
                .unwrap();
            r.extensions_mut()
                .insert(ConnectInfo(SocketAddr::from((peer, 5555))));
            r
        };
        let body = |req: axum::http::Request<axum::body::Body>| {
            let app = app.clone();
            async move {
                let res = app.oneshot(req).await.unwrap();
                String::from_utf8(
                    axum::body::to_bytes(res.into_body(), 1024)
                        .await
                        .unwrap()
                        .to_vec(),
                )
                .unwrap()
            }
        };
        assert_eq!(body(req([127, 0, 0, 1])).await, "trusted");
        assert_eq!(body(req([8, 8, 8, 8])).await, "untrusted");
    }
}
