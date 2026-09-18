// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

//! How many WebSocket bridges one project may hold open at once.
//!
//! ADVERSARY 011: an established bridge has no idle timeout, no lifetime cap and no per-project
//! connection cap. On the production `process` backend every tenant shares one machine and one
//! file-descriptor budget, so one project holding many silent-but-established bridges is a
//! cross-tenant availability problem rather than a self-inflicted one. 011 calls the cap the
//! highest-priority of its three recommendations, because it bounds the blast radius whatever the
//! idle story turns out to be.
//!
//! **Per replica, not global**, and that is stated rather than glossed: with N replicas the
//! effective ceiling is N times the configured one. The same honest caveat the rate limiters carry
//! (`docs/API.md`). A shared counter would need the database on every socket open, and the control
//! is a blast-radius bound rather than a quota — so the cheap version is the right trade, as long
//! as nobody reads the number as exact.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

#[derive(Clone, Default)]
pub struct BridgeCounter {
    live: Arc<Mutex<HashMap<Uuid, usize>>>,
}

/// Holds one slot. Releasing on `Drop` is the whole design: a bridge can end by a client
/// disconnect, an upstream close, a revocation, a lifetime cap, a panic in the pump, or a task
/// cancellation, and a counter decremented on any *particular* one of those paths would leak on
/// the others.
pub struct BridgeSlot {
    counter: BridgeCounter,
    project_id: Uuid,
}

impl Drop for BridgeSlot {
    fn drop(&mut self) {
        let mut live = match self.counter.live.lock() {
            Ok(g) => g,
            // A poisoned lock means another task panicked while holding it. Leaking one slot is
            // better than panicking in a destructor.
            Err(e) => e.into_inner(),
        };
        if let Some(n) = live.get_mut(&self.project_id) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                live.remove(&self.project_id);
            }
        }
    }
}

impl BridgeCounter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Take a slot, or `None` when the project is at its ceiling.
    pub fn acquire(&self, project_id: Uuid, max: usize) -> Option<BridgeSlot> {
        let mut live = match self.live.lock() {
            Ok(g) => g,
            Err(e) => e.into_inner(),
        };
        let n = live.entry(project_id).or_insert(0);
        if *n >= max {
            return None;
        }
        *n += 1;
        drop(live);
        Some(BridgeSlot {
            counter: self.clone(),
            project_id,
        })
    }

    #[cfg(test)]
    fn count(&self, project_id: &Uuid) -> usize {
        self.live
            .lock()
            .map(|l| l.get(project_id).copied().unwrap_or(0))
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cap_refuses_the_connection_past_it() {
        let c = BridgeCounter::new();
        let p = Uuid::new_v4();
        let a = c.acquire(p, 2).expect("first");
        let b = c.acquire(p, 2).expect("second");
        assert!(c.acquire(p, 2).is_none(), "the third must be refused");
        assert_eq!(c.count(&p), 2);
        drop(a);
        assert_eq!(c.count(&p), 1);
        assert!(c.acquire(p, 2).is_some(), "a freed slot is reusable");
        drop(b);
    }

    #[test]
    fn one_projects_bridges_do_not_consume_anothers() {
        let c = BridgeCounter::new();
        let (p, q) = (Uuid::new_v4(), Uuid::new_v4());
        let _a = c.acquire(p, 1).expect("p");
        assert!(c.acquire(p, 1).is_none());
        assert!(c.acquire(q, 1).is_some(), "q has its own budget");
    }

    #[test]
    fn a_released_project_stops_occupying_the_map() {
        let c = BridgeCounter::new();
        let p = Uuid::new_v4();
        {
            let _slot = c.acquire(p, 4).unwrap();
            assert_eq!(c.count(&p), 1);
        }
        assert_eq!(c.count(&p), 0);
        assert!(
            c.live.lock().unwrap().is_empty(),
            "an idle project must not keep a row: the map is unbounded in project count"
        );
    }

    #[test]
    fn a_cap_of_zero_admits_nothing() {
        let c = BridgeCounter::new();
        assert!(c.acquire(Uuid::new_v4(), 0).is_none());
    }
}
