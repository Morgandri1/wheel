//! Failed-bearer rate limiting for the host control plane.
//!
//! ADVERSARY review of the process backend: `:7100` needs a constant-time bearer *and* a rate
//! limit. The constant-time compare stops the secret leaking a byte at a time through response
//! timing; this stops an attacker simply trying secrets until one works.
//!
//! Why per-peer rather than global: a global counter would let anyone who can reach the port lock
//! the real API out by burning the budget deliberately. Keyed by peer address, a hostile sandbox
//! can only exhaust its own.
//!
//! In-memory is correct here, unlike the API's ingress limiter. The host is deliberately a single
//! instance — there is no second replica for a shared counter to coordinate with.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const WINDOW: Duration = Duration::from_secs(60);

pub struct AuthLimiter {
    max_failures_per_min: u32,
    state: Mutex<HashMap<IpAddr, Window>>,
}

struct Window {
    started: Instant,
    failures: u32,
}

impl AuthLimiter {
    pub fn new(max_failures_per_min: u32) -> Self {
        Self {
            max_failures_per_min,
            state: Mutex::new(HashMap::new()),
        }
    }

    /// True when this peer still has budget to attempt authentication.
    pub fn may_attempt(&self, peer: IpAddr) -> bool {
        if self.max_failures_per_min == 0 {
            return true;
        }
        let mut map = self.state.lock().unwrap_or_else(|e| e.into_inner());
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
    fn a_zero_budget_disables_the_limit() {
        let l = AuthLimiter::new(0);
        for _ in 0..50 {
            l.record_failure(ip(4));
            assert!(l.may_attempt(ip(4)));
        }
    }
}
