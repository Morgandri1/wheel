// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

//! What a closed usage window means for a turn, and when to try again
//! (docs/proposals/agentgrid-parity.md §1).
//!
//! Pure functions over values, so the whole policy is tested without a
//! process: which turn endings count as a limit, how long an agent parks, and
//! the bound that stops a limit requeue from looping.

use time::{Duration, OffsetDateTime};
use wheel_core::Timestamp;

/// A message whose turn keeps ending on a closed window is requeued at most
/// this many times, then consumed with an error. Each requeue already waits
/// for a real reset or a backoff of at least 15 minutes; this is the bound on
/// how often a misclassified body can come back at all.
pub const MAX_LIMIT_REQUEUES: u32 = 12;

/// Spread over which agents sharing one account come back after a reset.
pub const JITTER_SECS: std::ops::RangeInclusive<u64> = 5..=64;

const BACKOFF_BASE_SECS: i64 = 15 * 60;
const BACKOFF_MAX_SECS: i64 = 5 * 60 * 60;

/// The furthest ahead a resume may be scheduled: a weekly window plus a day.
/// A reset time beyond it is a bug or a forgery, and must not park an agent
/// indefinitely.
const HORIZON_SECS: i64 = 8 * 24 * 60 * 60;

/// Substrings of the harness's error text when the account's window closed.
///
/// The structured `rate_limit_event` is the primary signal. This fallback also
/// requires `is_error`, because the live wording is not verified in this repo
/// (proposal §1.2, honesty note).
const TEXT_MARKERS: &[&str] = &[
    "usage limit",
    "rate limit",
    "rate_limit_error",
    "hit your limit",
    "limit reached",
];

/// What the harness told us about a closed window.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LimitSignal {
    /// Unix seconds.
    pub resets_at: Option<i64>,
    pub window: Option<String>,
}

/// How a turn ended, for the purposes of the message it consumed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnEnd {
    Completed,
    /// A genuine task failure: the message is consumed with an error.
    TaskError,
    /// The harness has no usable credentials: requeue, wait for the operator.
    NeedsAuth,
    /// The account's usage window closed: requeue, park, resume at the reset.
    Limited(LimitSignal),
}

/// Classify a `result`.
///
/// `seen` is a `rejected` rate-limit event from THIS turn, already matched to
/// the session. A successful result is never a limit, even after one: the work
/// happened, on an overage window or otherwise.
pub fn classify_turn_end(
    is_error: bool,
    text: Option<&str>,
    seen: Option<&LimitSignal>,
    needs_auth: bool,
) -> TurnEnd {
    if !is_error {
        return TurnEnd::Completed;
    }
    let from_text = text.and_then(limit_in_text);
    if let Some(seen) = seen {
        let mut signal = seen.clone();
        if signal.resets_at.is_none() {
            signal.resets_at = from_text.and_then(|s| s.resets_at);
        }
        return TurnEnd::Limited(signal);
    }
    if needs_auth {
        return TurnEnd::NeedsAuth;
    }
    match from_text {
        Some(signal) => TurnEnd::Limited(signal),
        None => TurnEnd::TaskError,
    }
}

/// A limit named in the harness's error text, with the reset if it carries one.
pub fn limit_in_text(text: &str) -> Option<LimitSignal> {
    let lower = text.to_ascii_lowercase();
    if !TEXT_MARKERS.iter().any(|m| lower.contains(m)) {
        return None;
    }
    Some(LimitSignal {
        resets_at: epoch_after_pipe(text),
        window: None,
    })
}

/// The reset in `Claude AI usage limit reached|1757000000`: seconds, or
/// milliseconds read as seconds.
fn epoch_after_pipe(text: &str) -> Option<i64> {
    let (_, tail) = text.trim().rsplit_once('|')?;
    let digits: String = tail
        .trim()
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    let n: i64 = digits.parse().ok()?;
    match digits.len() {
        10 => Some(n),
        13 => Some(n / 1000),
        _ => None,
    }
}

/// How long to wait with no reset time: 15 minutes, doubling per strike, at
/// most five hours.
pub fn backoff_secs(strikes: u32) -> i64 {
    let doublings = strikes.saturating_sub(1).min(16);
    (BACKOFF_BASE_SECS << doublings).min(BACKOFF_MAX_SECS)
}

/// When a parked agent is due back.
pub fn resume_at(
    now: OffsetDateTime,
    resets_at: Option<i64>,
    strikes: u32,
    jitter_secs: u64,
) -> OffsetDateTime {
    let jitter = Duration::seconds(jitter_secs as i64);
    let at = match resets_at.and_then(|t| OffsetDateTime::from_unix_timestamp(t).ok()) {
        Some(reset) => reset.max(now) + jitter,
        None => now + Duration::seconds(backoff_secs(strikes)) + jitter,
    };
    at.min(now + Duration::seconds(HORIZON_SECS))
}

/// Unix seconds as a [`Timestamp`].
pub fn timestamp(unix: i64) -> Option<Timestamp> {
    OffsetDateTime::from_unix_timestamp(unix)
        .ok()
        .map(Timestamp::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_789_000_000;

    fn now() -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(NOW).unwrap()
    }

    fn seen(resets_at: Option<i64>) -> LimitSignal {
        LimitSignal {
            resets_at,
            window: Some("five_hour".into()),
        }
    }

    #[test]
    fn a_successful_turn_is_never_a_limit_even_after_a_rejected_event() {
        assert_eq!(
            classify_turn_end(false, Some("done"), Some(&seen(Some(NOW))), false),
            TurnEnd::Completed
        );
        assert_eq!(
            classify_turn_end(false, Some("Claude AI usage limit reached|1"), None, false),
            TurnEnd::Completed,
            "limit wording in a SUCCESSFUL result is the model's text, not the harness's verdict"
        );
    }

    #[test]
    fn a_rejected_event_in_the_turn_makes_an_error_a_limit() {
        assert_eq!(
            classify_turn_end(true, Some("boom"), Some(&seen(Some(NOW + 60))), false),
            TurnEnd::Limited(seen(Some(NOW + 60)))
        );
        // The event wins over an auth reading of the same text: the window is
        // the account-level fact the harness stated outright.
        assert_eq!(
            classify_turn_end(true, Some("Invalid API key"), Some(&seen(None)), true),
            TurnEnd::Limited(seen(None))
        );
    }

    #[test]
    fn the_text_supplies_the_reset_when_the_event_did_not() {
        let got = classify_turn_end(
            true,
            Some("Claude AI usage limit reached|1789003600"),
            Some(&seen(None)),
            false,
        );
        assert_eq!(got, TurnEnd::Limited(seen(Some(1_789_003_600))));
    }

    #[test]
    fn limit_wording_alone_in_an_error_is_a_limit() {
        for text in [
            "Claude AI usage limit reached|1789003600",
            "5-hour limit reached ∙ resets 3am",
            "You've hit your limit · resets 7pm (UTC)",
            "API Error: 429 {\"type\":\"error\",\"error\":{\"type\":\"rate_limit_error\"}}",
        ] {
            assert!(
                matches!(
                    classify_turn_end(true, Some(text), None, false),
                    TurnEnd::Limited(_)
                ),
                "{text:?}"
            );
        }
    }

    #[test]
    fn an_ordinary_error_is_a_task_error_and_auth_stays_auth() {
        assert_eq!(
            classify_turn_end(true, Some("the build failed"), None, false),
            TurnEnd::TaskError
        );
        assert_eq!(
            classify_turn_end(true, None, None, false),
            TurnEnd::TaskError
        );
        assert_eq!(
            classify_turn_end(true, Some("Not logged in"), None, true),
            TurnEnd::NeedsAuth
        );
    }

    #[test]
    fn only_a_plausible_epoch_after_the_pipe_is_a_reset() {
        assert_eq!(epoch_after_pipe("x|1789003600"), Some(1_789_003_600));
        assert_eq!(epoch_after_pipe("x|1789003600123"), Some(1_789_003_600));
        assert_eq!(epoch_after_pipe("x|42"), None);
        assert_eq!(epoch_after_pipe("no pipe 1789003600"), None);
        assert_eq!(epoch_after_pipe("x|soon"), None);
    }

    #[test]
    fn a_known_reset_resumes_at_the_reset_plus_jitter() {
        let at = resume_at(now(), Some(NOW + 3600), 1, 7);
        assert_eq!(at.unix_timestamp(), NOW + 3600 + 7);
    }

    #[test]
    fn a_reset_already_past_resumes_after_only_the_jitter() {
        let at = resume_at(now(), Some(NOW - 3600), 1, 5);
        assert_eq!(at.unix_timestamp(), NOW + 5);
    }

    #[test]
    fn a_far_future_reset_is_clamped_so_it_cannot_park_an_agent_for_ever() {
        let year_3000 = 32_503_680_000;
        let at = resume_at(now(), Some(year_3000), 1, 0);
        assert_eq!(at.unix_timestamp(), NOW + HORIZON_SECS);
    }

    #[test]
    fn no_reset_backs_off_from_fifteen_minutes_doubling_to_five_hours() {
        assert_eq!(backoff_secs(0), 900);
        assert_eq!(backoff_secs(1), 900);
        assert_eq!(backoff_secs(2), 1800);
        assert_eq!(backoff_secs(3), 3600);
        assert_eq!(backoff_secs(5), 4 * 3600);
        assert_eq!(backoff_secs(6), 5 * 3600, "capped");
        assert_eq!(backoff_secs(u32::MAX), 5 * 3600, "no overflow at the cap");
        assert_eq!(
            resume_at(now(), None, 2, 3).unix_timestamp(),
            NOW + 1800 + 3
        );
    }

    #[test]
    fn the_requeue_cap_and_jitter_are_the_documented_values() {
        assert_eq!(MAX_LIMIT_REQUEUES, 12);
        assert_eq!(JITTER_SECS, 5..=64);
        assert_eq!(timestamp(NOW).unwrap().into_inner().unix_timestamp(), NOW);
    }
}
