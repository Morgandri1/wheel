// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

//! Failed-bearer rate limiting for the host control plane.
//!
//! ADVERSARY review of the process backend: `:7100` needs a constant-time bearer *and* a rate
//! limit. The constant-time compare stops the secret leaking a byte at a time through response
//! timing; this stops an attacker simply trying secrets until one works.
//!
//! Why per-client rather than global: a global counter would let anyone who can reach the port lock
//! the real API out by burning the budget deliberately. Keyed by client address, a hostile caller
//! can only exhaust its own.
//!
//! "Client" is the TCP peer unless the operator names a reverse proxy in `WHEEL_TRUSTED_PROXIES`
//! (`wheel_core::client_ip`). It has to be nameable, because behind a public edge — the §5b
//! topology, where the API reaches this host over its public domain — the peer of EVERY caller,
//! `wheel-api` included, is the edge. Keyed on that peer this limiter would let anyone who learns
//! the domain spend the API's budget and lock the whole platform out of the host.
//!
//! In-memory is correct here, unlike the API's ingress limiter. The host is deliberately a single
//! instance — there is no second replica for a shared counter to coordinate with.

use axum::http::HeaderMap;
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use wheel_core::client_ip::TrustedProxies;

const WINDOW: Duration = Duration::from_secs(60);

/// The most distinct callers tracked at once. Past it, new callers share one bucket.
///
/// Keyed on an address the edge vouches for, the key space is whatever a caller can make it: a
/// rotating IPv6 /64, or a header the edge does not sanitise. Unbounded, an unauthenticated caller
/// could grow this map until the process that supervises every tenant runs out of memory.
const MAX_TRACKED: usize = 10_000;

/// Where callers past [`MAX_TRACKED`] are counted together.
/// The least time between two sweeps of a full map. Windows last a minute, so this loses nothing.
const SWEEP_EVERY: Duration = Duration::from_secs(1);

const OVERFLOW: IpAddr = IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED);

pub struct AuthLimiter {
    max_failures_per_min: u32,
    trusted: TrustedProxies,
    state: Mutex<HashMap<IpAddr, Window>>,
    /// When the map was last swept of expired entries while full. A sweep is a pass over every
    /// entry, so doing one per insert made a full map quadratic to fill (100k inserts took ~2 minutes).
    last_sweep: Mutex<Instant>,
}

struct Window {
    started: Instant,
    failures: u32,
}

impl AuthLimiter {
    pub fn new(max_failures_per_min: u32) -> Self {
        Self {
            max_failures_per_min,
            trusted: TrustedProxies::default(),
            state: Mutex::new(HashMap::new()),
            last_sweep: Mutex::new(
                Instant::now()
                    .checked_sub(SWEEP_EVERY)
                    .unwrap_or_else(Instant::now),
            ),
        }
    }

    /// Believe `X-Forwarded-For` from these proxies (and only these) when naming a caller.
    pub fn behind(mut self, trusted: TrustedProxies) -> Self {
        self.trusted = trusted;
        self
    }

    /// The address to charge a failure to: the vouched-for client, else the peer.
    pub fn client(&self, peer: IpAddr, headers: &HeaderMap) -> IpAddr {
        let values: Option<Vec<&str>> = headers
            .get_all("x-forwarded-for")
            .iter()
            .map(|v| v.to_str().ok())
            .collect();
        self.trusted.client(peer, &values.unwrap_or_default())
    }

    /// The entry a caller is counted under: its own, or the shared one once the map is full.
    fn key_for(map: &HashMap<IpAddr, Window>, peer: IpAddr) -> IpAddr {
        if map.contains_key(&peer) || map.len() < MAX_TRACKED {
            peer
        } else {
            OVERFLOW
        }
    }

    #[cfg(test)]
    fn tracked(&self) -> usize {
        self.state.lock().unwrap().len()
    }

    /// True when this peer still has budget to attempt authentication.
    pub fn may_attempt(&self, peer: IpAddr) -> bool {
        if self.max_failures_per_min == 0 {
            return true;
        }
        let mut map = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let peer = Self::key_for(&map, peer);
        match map.get(&peer) {
            Some(w) if w.started.elapsed() < WINDOW => w.failures < self.max_failures_per_min,
            // Window has closed; drop the stale entry so the map cannot grow without bound as
            // peers come and go.
            Some(_) => {
                map.remove(&peer);
                true
            }
            None => true,
        }
    }

    /// Record a rejected attempt. Successful authentications are deliberately not counted: a busy
    /// legitimate API must never be throttled.
    pub fn record_failure(&self, peer: IpAddr) {
        if self.max_failures_per_min == 0 {
            return;
        }
        let mut map = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if map.len() >= MAX_TRACKED && !map.contains_key(&peer) {
            // Full: drop what has expired before deciding anyone shares a bucket — but at most
            // once per SWEEP_EVERY, not per insert.
            let mut last = self.last_sweep.lock().unwrap_or_else(|e| e.into_inner());
            if last.elapsed() >= SWEEP_EVERY {
                map.retain(|_, w| w.started.elapsed() < WINDOW);
                *last = Instant::now();
            }
        }
        let peer = Self::key_for(&map, peer);
        let entry = map.entry(peer).or_insert_with(|| Window {
            started: Instant::now(),
            failures: 0,
        });
        if entry.started.elapsed() >= WINDOW {
            entry.started = Instant::now();
            entry.failures = 0;
        }
        entry.failures = entry.failures.saturating_add(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(last: u8) -> IpAddr {
        IpAddr::from([10, 0, 0, last])
    }

    #[test]
    fn budget_is_spent_by_failures_and_then_refuses() {
        let l = AuthLimiter::new(3);
        for _ in 0..3 {
            assert!(l.may_attempt(ip(1)));
            l.record_failure(ip(1));
        }
        assert!(
            !l.may_attempt(ip(1)),
            "a peer past its failure budget must be refused"
        );
    }

    #[test]
    fn one_hostile_peer_cannot_lock_out_another() {
        // The reason this is keyed by peer at all: a global counter would make denial of service
        // trivial for anyone who can reach the port.
        let l = AuthLimiter::new(2);
        for _ in 0..5 {
            l.record_failure(ip(1));
        }
        assert!(!l.may_attempt(ip(1)));
        assert!(
            l.may_attempt(ip(2)),
            "an unrelated peer was collaterally locked out"
        );
    }

    #[test]
    fn success_does_not_consume_budget() {
        // Only failures are recorded, so a busy legitimate caller is never throttled.
        let l = AuthLimiter::new(1);
        for _ in 0..100 {
            assert!(l.may_attempt(ip(3)));
        }
    }

    #[test]
    fn the_map_stays_bounded_however_many_distinct_callers_fail() {
        let l = AuthLimiter::new(3);
        for n in 0..(MAX_TRACKED as u128 * 10) {
            l.record_failure(IpAddr::from(n.to_be_bytes()));
        }
        assert!(
            l.tracked() <= MAX_TRACKED + 1,
            "{} entries tracked; a rotating key can OOM the host",
            l.tracked()
        );
    }

    #[test]
    fn past_the_cap_new_callers_share_one_bucket_and_known_ones_keep_their_own() {
        let l = AuthLimiter::new(2);
        let known = IpAddr::from([10, 9, 9, 9]);
        l.record_failure(known);
        for n in 0..(MAX_TRACKED as u32 + 50) {
            l.record_failure(IpAddr::from(n.to_be_bytes()));
        }
        // A caller first seen after the map filled is throttled with the others, not tracked alone.
        let late = IpAddr::from([203, 0, 113, 1]);
        for _ in 0..3 {
            l.record_failure(late);
        }
        assert!(!l.may_attempt(late), "the shared bucket should be spent");
        assert!(
            l.may_attempt(known),
            "a caller tracked before the cap kept its own budget"
        );
    }

    #[test]
    fn expired_entries_are_swept_when_the_map_is_full() {
        let l = AuthLimiter::new(3);
        {
            let mut map = l.state.lock().unwrap();
            for n in 0..MAX_TRACKED as u32 {
                map.insert(
                    IpAddr::from(n.to_be_bytes()),
                    Window {
                        started: Instant::now() - WINDOW * 2,
                        failures: 1,
                    },
                );
            }
        }
        let fresh = IpAddr::from([198, 51, 100, 7]);
        l.record_failure(fresh);
        assert_eq!(
            l.tracked(),
            1,
            "the stale entries should have been swept, leaving only the new one"
        );
    }

    #[test]
    fn a_zero_budget_disables_the_limit() {
        let l = AuthLimiter::new(0);
        for _ in 0..50 {
            l.record_failure(ip(4));
            assert!(l.may_attempt(ip(4)));
        }
    }
}
