//! The event bus behind `GET /v1/events`.
//!
//! A `broadcast` channel rather than per-subscriber queues, for one reason: a
//! slow or dead subscriber must never be able to stall the supervisor. A
//! browser that stops reading lags and is dropped; the delivery loop does not
//! notice. Publishing is non-blocking and ignores "no receivers", so the engine
//! behaves identically whether or not a UI is attached.

use tokio::sync::broadcast;
use wheel_core::Event;

/// How many events a slow subscriber may fall behind before it is dropped.
/// Large enough to absorb a burst of log lines, small enough that a dead
/// browser tab cannot pin megabytes.
const CAPACITY: usize = 1024;

#[derive(Clone)]
pub struct Bus {
    tx: broadcast::Sender<Event>,
}

impl Bus {
    pub fn new() -> Self {
        let (tx, _rx) = broadcast::channel(CAPACITY);
        Self { tx }
    }

    /// Publish. Never blocks, never fails: with no subscribers the send is a
    /// no-op, which is the normal case for a project with no UI attached.
    pub fn publish(&self, event: Event) {
        let _ = self.tx.send(event);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.tx.subscribe()
    }

    pub fn subscriber_count(&self) -> usize {
        self.tx.receiver_count()
    }
}

impl Default for Bus {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wheel_core::Timestamp;

    fn ev(n: u8) -> Event {
        Event::NodeState {
            node_id: uuid::Uuid::from_bytes([n; 16]),
            state: wheel_core::NodeState::Agent(Default::default()),
        }
    }

    #[tokio::test]
    async fn a_subscriber_receives_what_is_published() {
        let bus = Bus::new();
        let mut rx = bus.subscribe();
        bus.publish(ev(1));
        assert_eq!(rx.recv().await.unwrap(), ev(1));
    }

    #[tokio::test]
    async fn every_subscriber_gets_every_event() {
        let bus = Bus::new();
        let mut a = bus.subscribe();
        let mut b = bus.subscribe();
        bus.publish(ev(2));
        assert_eq!(a.recv().await.unwrap(), ev(2));
        assert_eq!(b.recv().await.unwrap(), ev(2));
    }

    /// The property that matters: publishing with nobody listening is a no-op,
    /// not an error. The engine must behave the same with and without a UI.
    #[tokio::test]
    async fn publishing_with_no_subscribers_is_harmless() {
        let bus = Bus::new();
        assert_eq!(bus.subscriber_count(), 0);
        bus.publish(ev(3));
        bus.publish(Event::BoardChanged {
            at: Timestamp::now(),
        });
    }

    /// A subscriber that stops reading must be dropped rather than allowed to
    /// stall the publisher — otherwise one dead browser tab would block the
    /// supervisor's delivery loop.
    #[tokio::test]
    async fn a_slow_subscriber_lags_and_never_blocks_the_publisher() {
        let bus = Bus::new();
        let mut slow = bus.subscribe();

        for i in 0..(CAPACITY + 50) {
            bus.publish(ev((i % 255) as u8));
        }

        match slow.recv().await {
            Err(broadcast::error::RecvError::Lagged(n)) => {
                assert!(
                    n > 0,
                    "the slow subscriber should be told how much it missed"
                );
            }
            other => panic!("expected Lagged, got {other:?}"),
        }
        // ...and it can carry on from the newest events rather than being
        // permanently broken.
        assert!(slow.recv().await.is_ok());
    }
}
