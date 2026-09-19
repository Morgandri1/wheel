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

/// Middleware: record the vouched-for client address on the request as [`ClientIp`].
pub async fn resolve(
    State(trusted): State<Arc<TrustedProxies>>,
    mut req: Request,
    next: Next,
) -> Response {
    if let Some(ConnectInfo(peer)) = req.extensions().get::<ConnectInfo<SocketAddr>>().copied() {
        let client = client_of(&trusted, peer.ip(), req.headers());
        req.extensions_mut().insert(ClientIp(client));
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
}
