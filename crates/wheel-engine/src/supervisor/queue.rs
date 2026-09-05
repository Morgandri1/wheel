//! Delivery queue selection (§3c#12 and the "Priority fairness" rule).
//!
//! Kept as a pure function over candidate rows so the fairness rules can be
//! tested exhaustively without a database, a process or a clock.

use uuid::Uuid;
use wheel_core::Timestamp;

/// Which lane a queued message is in. User messages jump the queue but must not
/// be able to starve agent traffic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lane {
    /// `from = user` — the operator's chat box.
    User,
    /// Everything else: agent, endpoint, script.
    Normal,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub id: Uuid,
    pub lane: Lane,
    pub created_at: Timestamp,
}

/// Contract constants (§3 "Priority fairness").
pub const MAX_CONSECUTIVE_USER: u32 = 3;
pub const NORMAL_PROMOTE_AFTER_SECS: i64 = 60;

/// Choose the next message to deliver.
///
/// Rules, in order:
/// 1. A normal-lane message older than 60s is promoted to the front — this is
///    what stops operator chatter from starving agent traffic.
/// 2. After 3 consecutive user messages, one normal-lane message goes next.
/// 3. Otherwise the user lane wins.
/// 4. Within a lane, oldest first.
///
/// `now` is passed in rather than read, so the aging rule is testable.
pub fn next_message(
    queued: &[Candidate],
    consecutive_user: u32,
    now: Timestamp,
) -> Option<&Candidate> {
    if queued.is_empty() {
        return None;
    }

    let oldest_in = |lane: Lane| -> Option<&Candidate> {
        queued
            .iter()
            .filter(|c| c.lane == lane)
            .min_by_key(|c| c.created_at)
    };

    let user = oldest_in(Lane::User);
    let normal = oldest_in(Lane::Normal);

    // 1. Aging beats everything: a normal message that has waited too long is
    //    promoted regardless of what the user lane holds.
    if let Some(n) = normal {
        let waited = now.into_inner() - n.created_at.into_inner();
        if waited.whole_seconds() >= NORMAL_PROMOTE_AFTER_SECS {
            return Some(n);
        }
    }

    // 2. Fairness cap: the user lane cannot run away with the turn budget.
    if consecutive_user >= MAX_CONSECUTIVE_USER {
        if let Some(n) = normal {
            return Some(n);
        }
    }

    // 3./4. User lane first, else oldest normal.
    user.or(normal)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(secs: i64) -> Timestamp {
        Timestamp::parse_rfc3339("2026-09-05T00:00:00Z")
            .unwrap()
            .into_inner()
            .saturating_add(time::Duration::seconds(secs))
            .into()
    }

    fn c(n: u8, lane: Lane, at: i64) -> Candidate {
        Candidate {
            id: Uuid::from_bytes([n; 16]),
            lane,
            created_at: ts(at),
        }
    }

    #[test]
    fn an_empty_queue_yields_nothing() {
        assert!(next_message(&[], 0, ts(0)).is_none());
    }

    #[test]
    fn the_user_lane_goes_first() {
        // The agent message is OLDER, and the user's still wins: that is the
        // point of the priority lane.
        let q = vec![c(1, Lane::Normal, 0), c(2, Lane::User, 10)];
        assert_eq!(
            next_message(&q, 0, ts(20)).unwrap().id,
            c(2, Lane::User, 10).id
        );
    }

    #[test]
    fn within_a_lane_the_oldest_goes_first() {
        let q = vec![
            c(1, Lane::User, 30),
            c(2, Lane::User, 10),
            c(3, Lane::User, 20),
        ];
        assert_eq!(
            next_message(&q, 0, ts(40)).unwrap().id,
            c(2, Lane::User, 10).id
        );
    }

    #[test]
    fn three_consecutive_user_messages_then_one_agent_message() {
        let q = vec![c(1, Lane::User, 10), c(2, Lane::Normal, 11)];
        // Under the cap, the user keeps winning.
        for consecutive in 0..MAX_CONSECUTIVE_USER {
            assert_eq!(
                next_message(&q, consecutive, ts(20)).unwrap().id,
                c(1, Lane::User, 10).id,
                "user should still win at {consecutive} consecutive"
            );
        }
        // At the cap, the normal lane gets a turn.
        assert_eq!(
            next_message(&q, MAX_CONSECUTIVE_USER, ts(20)).unwrap().id,
            c(2, Lane::Normal, 11).id
        );
    }

    #[test]
    fn the_cap_does_not_stall_delivery_when_only_user_messages_exist() {
        // Hitting the cap with nothing in the normal lane must not return None,
        // or the queue would deadlock until an agent happened to send something.
        let q = vec![c(1, Lane::User, 10)];
        assert_eq!(
            next_message(&q, MAX_CONSECUTIVE_USER + 5, ts(20))
                .unwrap()
                .id,
            c(1, Lane::User, 10).id
        );
    }

    #[test]
    fn a_normal_message_older_than_60s_is_promoted_over_the_user_lane() {
        let q = vec![c(1, Lane::Normal, 0), c(2, Lane::User, 100)];
        // At 59s the user still wins.
        assert_eq!(
            next_message(&q, 0, ts(59)).unwrap().id,
            c(2, Lane::User, 100).id
        );
        // At exactly 60s the aged message is promoted.
        assert_eq!(
            next_message(&q, 0, ts(60)).unwrap().id,
            c(1, Lane::Normal, 0).id
        );
        assert_eq!(
            next_message(&q, 0, ts(600)).unwrap().id,
            c(1, Lane::Normal, 0).id
        );
    }

    #[test]
    fn aging_picks_the_oldest_normal_message_not_merely_an_aged_one() {
        let q = vec![
            c(1, Lane::Normal, 10),
            c(2, Lane::Normal, 0),
            c(3, Lane::User, 100),
        ];
        assert_eq!(
            next_message(&q, 0, ts(100)).unwrap().id,
            c(2, Lane::Normal, 0).id
        );
    }

    #[test]
    fn a_lone_normal_message_is_delivered_without_waiting_to_age() {
        let q = vec![c(1, Lane::Normal, 0)];
        assert_eq!(
            next_message(&q, 0, ts(1)).unwrap().id,
            c(1, Lane::Normal, 0).id
        );
    }
}
