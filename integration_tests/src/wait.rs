//   Copyright 2026 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

//! Condition-based waiting for steps that observe asynchronous progress in spawned processes.
//!
//! Every bound passes through [`scaled`], so a slow runner stretches all of them at once through
//! [`TIMEOUT_SCALE_ENV`] rather than each step carrying its own allowance for load.

use std::{
    fmt::{Debug, Display},
    ops::ControlFlow,
    sync::LazyLock,
    time::Duration,
};

use tokio::time::Instant;

/// A positive multiplier applied to every timeout handed to [`wait_until`], e.g. `2` or `0.5`.
pub const TIMEOUT_SCALE_ENV: &str = "CUCUMBER_TIMEOUT_SCALE";

/// The interval before the second poll. It doubles after every unsatisfied poll up to [`MAX_POLL_INTERVAL`], so a
/// condition that is nearly met is seen quickly and a long wait does not flood the process with requests.
pub const INITIAL_POLL_INTERVAL: Duration = Duration::from_millis(100);
pub const MAX_POLL_INTERVAL: Duration = Duration::from_secs(2);

static TIMEOUT_SCALE: LazyLock<f64> =
    LazyLock::new(|| parse_timeout_scale(std::env::var(TIMEOUT_SCALE_ENV).ok().as_deref()));

fn parse_timeout_scale(value: Option<&str>) -> f64 {
    let Some(value) = value else {
        return 1.0;
    };
    match value.trim().parse::<f64>() {
        Ok(scale) if scale.is_finite() && scale > 0.0 => scale,
        _ => panic!("{TIMEOUT_SCALE_ENV} must be a positive number, got '{value}'"),
    }
}

/// Applies the [`TIMEOUT_SCALE_ENV`] multiplier to `timeout`.
pub fn scaled(timeout: Duration) -> Duration {
    timeout.mul_f64(*TIMEOUT_SCALE)
}

/// The condition was still unmet when the deadline passed.
#[derive(Debug)]
pub struct TimedOut<S> {
    /// The scaled timeout that elapsed.
    pub timeout: Duration,
    /// What the final poll observed.
    pub last: S,
}

impl<S: Debug> Display for TimedOut<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "timed out after {:.1?} (last observed: {:?})",
            self.timeout, self.last
        )
    }
}

/// Polls `poll` until it returns `Break`, backing off between polls, for up to `timeout` scaled by
/// [`TIMEOUT_SCALE_ENV`].
///
/// `poll` returns `Continue` with what it observed while the condition is unmet; the last observation is returned
/// in [`TimedOut`] so the failure can say how far the wait got. `poll` runs at least once and once more at the
/// deadline, so a condition met during the final interval is still seen.
pub async fn wait_until<T, S>(
    timeout: Duration,
    mut poll: impl AsyncFnMut() -> ControlFlow<T, S>,
) -> Result<T, TimedOut<S>> {
    let timeout = scaled(timeout);
    let deadline = Instant::now() + timeout;
    let mut interval = INITIAL_POLL_INTERVAL;
    loop {
        let last = match poll().await {
            ControlFlow::Break(value) => return Ok(value),
            ControlFlow::Continue(last) => last,
        };
        let now = Instant::now();
        if now >= deadline {
            return Err(TimedOut { timeout, last });
        }
        tokio::time::sleep(interval.min(deadline - now)).await;
        interval = (interval * 2).min(MAX_POLL_INTERVAL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn it_returns_as_soon_as_the_condition_holds() {
        let start = Instant::now();
        let mut polls = 0;
        let result = wait_until(Duration::from_secs(60), async || {
            polls += 1;
            if polls == 3 {
                ControlFlow::Break(polls)
            } else {
                ControlFlow::Continue(())
            }
        })
        .await
        .unwrap();

        assert_eq!(result, 3);
        assert_eq!(start.elapsed(), INITIAL_POLL_INTERVAL * 3);
    }

    #[tokio::test(start_paused = true)]
    async fn it_times_out_at_the_deadline_with_the_last_observation() {
        let start = Instant::now();
        let mut polls = 0u32;
        let err = wait_until(Duration::from_secs(10), async || {
            polls += 1;
            ControlFlow::<(), _>::Continue(polls)
        })
        .await
        .unwrap_err();

        assert_eq!(start.elapsed(), Duration::from_secs(10));
        assert_eq!(err.timeout, Duration::from_secs(10));
        assert_eq!(err.last, polls);
        // 100ms, 200ms, 400ms, 800ms, 1.6s, then 2s intervals until the final poll at the deadline
        assert_eq!(polls, 10);
    }

    #[tokio::test(start_paused = true)]
    async fn it_polls_once_with_a_zero_timeout() {
        let mut polls = 0;
        let err = wait_until(Duration::ZERO, async || {
            polls += 1;
            ControlFlow::<(), _>::Continue("pending")
        })
        .await
        .unwrap_err();

        assert_eq!(polls, 1);
        assert_eq!(err.last, "pending");
    }

    #[test]
    fn it_parses_the_timeout_scale() {
        assert_eq!(parse_timeout_scale(None), 1.0);
        assert_eq!(parse_timeout_scale(Some("2.5")), 2.5);
        assert_eq!(parse_timeout_scale(Some(" 3 ")), 3.0);
    }

    #[test]
    #[should_panic(expected = "must be a positive number")]
    fn it_rejects_a_zero_timeout_scale() {
        parse_timeout_scale(Some("0"));
    }

    #[test]
    #[should_panic(expected = "must be a positive number")]
    fn it_rejects_a_malformed_timeout_scale() {
        parse_timeout_scale(Some("fast"));
    }
}
