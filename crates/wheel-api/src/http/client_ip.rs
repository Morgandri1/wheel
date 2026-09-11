// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

//! The address a request really came from.
//!
//! Behind a reverse proxy the TCP peer is the proxy, and the caller is named in `X-Forwarded-For`
//! — by the proxy, or by anyone who wants to be someone else, because a client can send that header
//! itself. So the header is believed only when the peer is a proxy the operator named in
//! `WHEEL_TRUSTED_PROXIES`, and then only as far back as the chain stays inside those proxies: the
//! first address from the right that is not one of them is the client. With no trusted proxies,
//! the default, the peer is the client and the header is ignored.
//!
//! `X-Forwarded-Proto` is not read at all. The scheme the API advertises comes from
//! `PUBLIC_BASE_URL`, which is configuration rather than a claim a request can make.

use axum::extract::{ConnectInfo, Request, State};
use axum::http::HeaderMap;
use axum::middleware::Next;
use axum::response::Response;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

pub const ENV_TRUSTED_PROXIES: &str = "WHEEL_TRUSTED_PROXIES";

/// The caller's address, as far as this API can vouch for it. Present on a request when the server
/// was built with connect info and [`resolve`] ran.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientIp(pub IpAddr);

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrustedProxies(Vec<Cidr>);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Cidr {
    net: IpAddr,
    bits: u8,
}

impl Cidr {
    fn parse(raw: &str) -> Result<Self, String> {
        let (addr, bits) = match raw.split_once('/') {
            Some((a, b)) => (a, Some(b)),
            None => (raw, None),
        };
        let net: IpAddr = addr
            .trim()
            .parse()
            .map_err(|_| format!("{raw:?} is not an address or a CIDR"))?;
        let net = canonical(net);
        let max = if net.is_ipv4() { 32 } else { 128 };
        let bits = match bits {
            None => max,
            Some(b) => b
                .trim()
                .parse::<u8>()
                .ok()
                .filter(|b| *b <= max)
                .ok_or_else(|| format!("{raw:?} has an invalid prefix length"))?,
        };
        Ok(Self { net, bits })
    }

    fn contains(&self, ip: IpAddr) -> bool {
        match (self.net, canonical(ip)) {
            (IpAddr::V4(n), IpAddr::V4(a)) => {
                same_prefix(u32::from(n).into(), u32::from(a).into(), self.bits, 32)
            }
            (IpAddr::V6(n), IpAddr::V6(a)) => same_prefix(n.into(), a.into(), self.bits, 128),
            _ => false,
        }
    }
}

fn same_prefix(a: u128, b: u128, bits: u8, width: u32) -> bool {
    let shift = width - u32::from(bits);
    shift >= 128 || (a >> shift) == (b >> shift)
}

/// `::ffff:127.0.0.1` is 127.0.0.1: a dual-stack listener reports IPv4 peers in that form.
fn canonical(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(ip),
        v4 => v4,
    }
}

impl TrustedProxies {
    pub fn from_env() -> Result<Self, String> {
        Self::parse(&std::env::var(ENV_TRUSTED_PROXIES).unwrap_or_default())
    }

    /// Comma-separated addresses or CIDRs, e.g. `127.0.0.1/32, ::1`.
    pub fn parse(raw: &str) -> Result<Self, String> {
        raw.split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| Cidr::parse(s).map_err(|e| format!("{ENV_TRUSTED_PROXIES}: {e}")))
            .collect::<Result<Vec<_>, _>>()
            .map(Self)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    fn trusts(&self, ip: IpAddr) -> bool {
        self.0.iter().any(|c| c.contains(ip))
    }

    /// The client behind `peer`. Walks `X-Forwarded-For` from the right only while each hop is a
    /// trusted proxy; anything a client could have written itself is never reached. A hop that is
    /// not an address ends the walk at the last one that could be vouched for.
    pub fn client(&self, peer: IpAddr, headers: &HeaderMap) -> IpAddr {
        let mut client = canonical(peer);
        if !self.trusts(client) {
            return client;
        }
        let mut hops = Vec::new();
        for value in headers.get_all("x-forwarded-for") {
            let Ok(text) = value.to_str() else {
                return client;
            };
            hops.extend(text.split(',').map(str::trim));
        }
        for hop in hops.iter().rev() {
            let Ok(ip) = hop.parse::<IpAddr>() else {
                return client;
            };
            client = canonical(ip);
            if !self.trusts(client) {
                return client;
            }
        }
        client
    }
}

/// Middleware: record the vouched-for client address on the request as [`ClientIp`].
pub async fn resolve(
    State(trusted): State<Arc<TrustedProxies>>,
    mut req: Request,
    next: Next,
) -> Response {
    if let Some(ConnectInfo(peer)) = req.extensions().get::<ConnectInfo<SocketAddr>>().copied() {
        let client = trusted.client(peer.ip(), req.headers());
        req.extensions_mut().insert(ClientIp(client));
    }
    next.run(req).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    fn xff(values: &[&str]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for v in values {
            h.append("x-forwarded-for", v.parse().unwrap());
        }
        h
    }

    #[test]
    fn nothing_is_trusted_by_default() {
        let none = TrustedProxies::parse("").unwrap();
        assert!(none.is_empty());
        assert_eq!(
            none.client(ip("127.0.0.1"), &xff(&["198.51.100.7"])),
            ip("127.0.0.1")
        );
    }

    #[test]
    fn a_header_from_an_untrusted_peer_is_ignored() {
        let t = TrustedProxies::parse("127.0.0.1/32").unwrap();
        assert_eq!(
            t.client(ip("203.0.113.9"), &xff(&["1.1.1.1"])),
            ip("203.0.113.9")
        );
    }

    #[test]
    fn a_trusted_proxy_names_the_client_and_a_prepended_lie_is_never_reached() {
        let t = TrustedProxies::parse("127.0.0.1/32").unwrap();
        assert_eq!(
            t.client(ip("127.0.0.1"), &xff(&["198.51.100.7"])),
            ip("198.51.100.7")
        );
        assert_eq!(
            t.client(ip("127.0.0.1"), &xff(&["6.6.6.6, 198.51.100.7"])),
            ip("198.51.100.7")
        );
        assert_eq!(
            t.client(ip("127.0.0.1"), &xff(&["6.6.6.6", "198.51.100.7"])),
            ip("198.51.100.7"),
            "repeated headers are one list"
        );
    }

    #[test]
    fn a_chain_of_trusted_proxies_is_walked_through() {
        let t = TrustedProxies::parse(" 127.0.0.1 , 10.0.0.0/8 ").unwrap();
        assert_eq!(
            t.client(ip("127.0.0.1"), &xff(&["198.51.100.7, 10.1.2.3"])),
            ip("198.51.100.7")
        );
        assert_eq!(
            t.client(ip("127.0.0.1"), &xff(&["10.9.9.9"])),
            ip("10.9.9.9")
        );
    }

    #[test]
    fn missing_or_malformed_hops_stop_at_what_can_be_vouched_for() {
        let t = TrustedProxies::parse("127.0.0.1").unwrap();
        assert_eq!(
            t.client(ip("127.0.0.1"), &HeaderMap::new()),
            ip("127.0.0.1")
        );
        assert_eq!(
            t.client(ip("127.0.0.1"), &xff(&["not-an-ip"])),
            ip("127.0.0.1")
        );
        assert_eq!(
            t.client(ip("127.0.0.1"), &xff(&["1.2.3.4:5678"])),
            ip("127.0.0.1")
        );
        let mut binary = HeaderMap::new();
        binary.insert(
            "x-forwarded-for",
            axum::http::HeaderValue::from_bytes(b"\xff").unwrap(),
        );
        assert_eq!(t.client(ip("127.0.0.1"), &binary), ip("127.0.0.1"));
    }

    #[test]
    fn ipv6_and_mapped_addresses_match_their_networks() {
        let t = TrustedProxies::parse("fd00::/8, 127.0.0.1").unwrap();
        assert_eq!(
            t.client(ip("fd12::1"), &xff(&["2001:db8::5"])),
            ip("2001:db8::5")
        );
        assert_eq!(
            t.client(ip("::ffff:127.0.0.1"), &xff(&["198.51.100.7"])),
            ip("198.51.100.7")
        );
        assert_eq!(
            t.client(ip("fe80::1"), &xff(&["198.51.100.7"])),
            ip("fe80::1")
        );
        let all = TrustedProxies::parse("0.0.0.0/0").unwrap();
        assert_eq!(
            all.client(ip("9.9.9.9"), &xff(&["198.51.100.7"])),
            ip("198.51.100.7")
        );
    }

    #[test]
    fn a_bad_entry_refuses_to_configure_and_names_the_variable() {
        for bad in ["nope", "10.0.0.0/33", "::1/129", "10.0.0.0/x"] {
            let e = TrustedProxies::parse(bad).unwrap_err();
            assert!(e.contains(ENV_TRUSTED_PROXIES) && e.contains(bad), "{e}");
        }
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
