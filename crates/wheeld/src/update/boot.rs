// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

//! The first thing a wheeld does after an update: decide, from the pending
//! marker, whether it is the new binary on probation, a new binary that
//! already crashed once, or an old one whose swap never happened.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::state::Pending;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Boot {
    Normal,
    /// The new binary's first boot: it has `health_for` to prove itself.
    Probation(Pending),
    /// The new binary booting again without having proved healthy, so it
    /// crashed or was killed on the way. Roll back before anything runs.
    RollBack(Pending),
    /// This binary is not the one the marker installed: the swap never landed.
    Interrupted(Pending),
}

pub fn decide(pending: Option<&Pending>, running: &str) -> Boot {
    match pending {
        None => Boot::Normal,
        Some(p) if p.to != running => Boot::Interrupted(p.clone()),
        Some(p) if p.attempts >= 1 => Boot::RollBack(p.clone()),
        Some(p) => Boot::Probation(p.clone()),
    }
}

/// Runs `on_timeout` unless `healthy` is set within `deadline`.
///
/// A plain OS thread rather than a task: the failure this exists for includes
/// a new binary whose async runtime is wedged, and a watchdog that needs that
/// runtime would be waiting on the thing it is watching.
pub fn watchdog(
    deadline: Duration,
    healthy: Arc<AtomicBool>,
    on_timeout: impl FnOnce() + Send + 'static,
) -> std::thread::JoinHandle<bool> {
    std::thread::spawn(move || {
        let until = Instant::now() + deadline;
        while Instant::now() < until {
            if healthy.load(Ordering::SeqCst) {
                return false;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        if healthy.load(Ordering::SeqCst) {
            return false;
        }
        on_timeout();
        true
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::update::state::Requester;

    fn pending(attempts: u32) -> Pending {
        Pending {
            from: "a".repeat(40),
            to: "b".repeat(40),
            by: Requester::Operator,
            components: vec![],
            attempts,
            at: 0,
        }
    }

    #[test]
    fn the_marker_decides_what_this_boot_is() {
        let new = "b".repeat(40);
        let old = "a".repeat(40);
        assert_eq!(decide(None, &new), Boot::Normal);
        assert_eq!(decide(Some(&pending(0)), &new), Boot::Probation(pending(0)));
        assert_eq!(decide(Some(&pending(1)), &new), Boot::RollBack(pending(1)));
        assert_eq!(
            decide(Some(&pending(0)), &old),
            Boot::Interrupted(pending(0))
        );
        assert_eq!(
            decide(Some(&pending(3)), &old),
            Boot::Interrupted(pending(3))
        );
    }

    #[test]
    fn a_watchdog_that_is_not_told_the_binary_is_healthy_fires() {
        let fired = Arc::new(AtomicBool::new(false));
        let flag = fired.clone();
        let handle = watchdog(
            Duration::from_millis(100),
            Arc::new(AtomicBool::new(false)),
            move || flag.store(true, Ordering::SeqCst),
        );
        assert!(handle.join().unwrap());
        assert!(fired.load(Ordering::SeqCst));
    }

    #[test]
    fn a_healthy_binary_stops_its_watchdog() {
        let healthy = Arc::new(AtomicBool::new(false));
        let handle = watchdog(Duration::from_secs(30), healthy.clone(), || {
            panic!("rolled back a healthy binary")
        });
        healthy.store(true, Ordering::SeqCst);
        assert!(!handle.join().unwrap());

        let late = watchdog(Duration::ZERO, Arc::new(AtomicBool::new(true)), || {
            panic!("rolled back a binary that proved healthy in time")
        });
        assert!(!late.join().unwrap());
    }
}
