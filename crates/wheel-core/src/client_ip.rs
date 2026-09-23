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
//! Shared by the API (ingress rate limits) and the sandbox host (failed-bearer throttling) so the
//! two cannot come to disagree about who a caller is. Pure apart from [`TrustedProxies::from_env`].

use std::net::IpAddr;

pub const ENV_TRUSTED_PROXIES: &str = "WHEEL_TRUSTED_PROXIES";

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

    /// Does any entry match **every** address of its family — `0.0.0.0/0`, `::/0`, or a mapped
    /// spelling of either?
    ///
    /// A list that contains one is, for trust purposes, the same list as an empty one wearing a
    /// configuration's clothes: [`trusts_peer`](Self::trusts_peer) returns true for the whole
    /// internet. A caller deciding whether "no trusted proxies configured" should refuse to boot
    /// (an empty-list check alone) must ask this too, or a value that is not empty, still parses,
    /// and still trusts everyone slips past it.
    ///
    /// `bits == 0` is the whole predicate. It is the only prefix length that is never a correct
    /// answer for an authenticating proxy; `10.0.0.0/8` is a legitimate one, so this is not a
    /// private-range check and must not become one.
    pub fn covers_every_address(&self) -> bool {
        self.0.iter().any(|c| c.bits == 0)
    }

    /// Is this address one of the operator's proxies?
    ///
    /// Named for the peer because that is the only address it may ever be asked about directly: an
    /// `X-Forwarded-For` hop is a claim, and asking whether a claim is trusted is how a header
    /// becomes an identity — that walk stays inside [`client`](Self::client).
    pub fn trusts_peer(&self, ip: IpAddr) -> bool {
        self.0.iter().any(|c| c.contains(ip))
    }

    /// The client behind `peer`. `forwarded_for` is every `X-Forwarded-For` header value, in order;
    /// each may itself be a comma-separated list. Walks the hops from the right only while each one
    /// is a trusted proxy; anything a client could have written itself is never reached. A hop that
    /// is not an address ends the walk at the last one that could be vouched for.
    pub fn client(&self, peer: IpAddr, forwarded_for: &[&str]) -> IpAddr {
        let mut client = canonical(peer);
        if !self.trusts_peer(client) {
            return client;
        }
        let hops: Vec<&str> = forwarded_for
            .iter()
            .flat_map(|value| value.split(',').map(str::trim))
            .collect();
        for hop in hops.iter().rev() {
            let Ok(ip) = hop.parse::<IpAddr>() else {
                return client;
            };
            client = canonical(ip);
            if !self.trusts_peer(client) {
                return client;
            }
        }
        client
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn nothing_is_trusted_by_default() {
        let none = TrustedProxies::parse("").unwrap();
        assert!(none.is_empty());
        assert_eq!(
            none.client(ip("127.0.0.1"), &["198.51.100.7"]),
            ip("127.0.0.1")
        );
    }

    #[test]
    fn a_header_from_an_untrusted_peer_is_ignored() {
        let t = TrustedProxies::parse("127.0.0.1/32").unwrap();
        assert_eq!(t.client(ip("203.0.113.9"), &["1.1.1.1"]), ip("203.0.113.9"));
    }

    #[test]
    fn a_trusted_proxy_names_the_client_and_a_prepended_lie_is_never_reached() {
        let t = TrustedProxies::parse("127.0.0.1/32").unwrap();
        assert_eq!(
            t.client(ip("127.0.0.1"), &["198.51.100.7"]),
            ip("198.51.100.7")
        );
        assert_eq!(
            t.client(ip("127.0.0.1"), &["6.6.6.6, 198.51.100.7"]),
            ip("198.51.100.7")
        );
        assert_eq!(
            t.client(ip("127.0.0.1"), &["6.6.6.6", "198.51.100.7"]),
            ip("198.51.100.7"),
            "repeated headers are one list"
        );
    }

    #[test]
    fn a_chain_of_trusted_proxies_is_walked_through() {
        let t = TrustedProxies::parse(" 127.0.0.1 , 10.0.0.0/8 ").unwrap();
        assert_eq!(
            t.client(ip("127.0.0.1"), &["198.51.100.7, 10.1.2.3"]),
            ip("198.51.100.7")
        );
        assert_eq!(t.client(ip("127.0.0.1"), &["10.9.9.9"]), ip("10.9.9.9"));
    }

    #[test]
    fn missing_or_malformed_hops_stop_at_what_can_be_vouched_for() {
        let t = TrustedProxies::parse("127.0.0.1").unwrap();
        assert_eq!(t.client(ip("127.0.0.1"), &[]), ip("127.0.0.1"));
        assert_eq!(t.client(ip("127.0.0.1"), &["not-an-ip"]), ip("127.0.0.1"));
        assert_eq!(
            t.client(ip("127.0.0.1"), &["1.2.3.4:5678"]),
            ip("127.0.0.1")
        );
    }

    #[test]
    fn ipv6_and_mapped_addresses_match_their_networks() {
        let t = TrustedProxies::parse("fd00::/8, 127.0.0.1").unwrap();
        assert_eq!(t.client(ip("fd12::1"), &["2001:db8::5"]), ip("2001:db8::5"));
        assert_eq!(
            t.client(ip("::ffff:127.0.0.1"), &["198.51.100.7"]),
            ip("198.51.100.7")
        );
        assert_eq!(t.client(ip("fe80::1"), &["198.51.100.7"]), ip("fe80::1"));
        let all = TrustedProxies::parse("0.0.0.0/0").unwrap();
        assert_eq!(
            all.client(ip("9.9.9.9"), &["198.51.100.7"]),
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

    /// `0.0.0.0/0` is not empty, parses, and trusts the internet — a different question from
    /// `is_empty`, and a caller checking only the latter would let it through.
    #[test]
    fn covers_every_address_is_true_only_for_a_true_wildcard() {
        for wildcard in ["0.0.0.0/0", "::/0", "0.0.0.0/0,::/0", "10.0.0.0/8, ::/0"] {
            assert!(
                TrustedProxies::parse(wildcard)
                    .unwrap()
                    .covers_every_address(),
                "{wildcard}"
            );
        }
        for real in ["", "10.0.0.0/8", "127.0.0.1", "127.0.0.1/32, 10.0.0.0/8"] {
            assert!(
                !TrustedProxies::parse(real).unwrap().covers_every_address(),
                "{real}"
            );
        }
    }

    #[test]
    fn trusts_peer_answers_for_the_single_address_asked_about() {
        let t = TrustedProxies::parse("127.0.0.1/32, 10.0.0.0/8").unwrap();
        assert!(t.trusts_peer(ip("127.0.0.1")));
        assert!(t.trusts_peer(ip("10.5.5.5")));
        assert!(!t.trusts_peer(ip("8.8.8.8")));
    }
}
