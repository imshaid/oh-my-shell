//! Trying a request again after a failure that may pass (blueprint §13.3).

use std::future::Future;
use std::time::Duration;

use crate::error::ProviderError;

/// The longest `Retry-After` that is waited out. A server that asks for more (a quota that
/// resets in an hour) is not waited for: sitting at a prompt for minutes is worse than the error
/// line, which says how long the server asked.
pub const LONGEST_WAIT: Duration = Duration::from_secs(10);

/// How many tries, and how long to wait between them.
#[derive(Clone, Debug)]
pub struct Policy {
    /// Total tries, the first one included. At least 1.
    pub attempts: u32,
    /// The wait before the second try, the third, ... The last value repeats if more are
    /// needed.
    pub pauses: Vec<Duration>,
}

impl Policy {
    /// Three tries, waiting half a second and then a second.
    #[must_use]
    pub fn standard() -> Self {
        Self {
            attempts: 3,
            pauses: vec![Duration::from_millis(500), Duration::from_secs(1)],
        }
    }

    /// One try.
    #[must_use]
    pub fn once() -> Self {
        Self {
            attempts: 1,
            pauses: Vec::new(),
        }
    }

    pub(crate) fn pause(&self, failures: u32) -> Duration {
        let index = (failures.saturating_sub(1)) as usize;
        self.pauses
            .get(index)
            .or_else(|| self.pauses.last())
            .copied()
            .unwrap_or_default()
    }
}

/// How long to wait before the next try after `failures` failures, or `None` when the server
/// asked for more than [`LONGEST_WAIT`] and the failure should be reported instead. The
/// server's request is a floor: the policy's own pause is used when it is longer.
pub(crate) fn wait_before_retry(
    policy: &Policy,
    failures: u32,
    error: &ProviderError,
) -> Option<Duration> {
    let asked = error.retry_after.unwrap_or_default();
    (asked <= LONGEST_WAIT).then(|| policy.pause(failures).max(asked))
}

/// Runs `attempt` until it works, fails for good, or the tries are used up. Only a failure
/// that [`ProviderError::is_transient`] is tried again; a wrong key or a Ctrl+C returns at
/// once. The last error is returned.
///
/// # Errors
///
/// The error of the last try.
pub async fn retry<T, F, Fut>(policy: &Policy, mut attempt: F) -> Result<T, ProviderError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, ProviderError>>,
{
    let mut failures = 0;
    loop {
        match attempt().await {
            Ok(value) => return Ok(value),
            Err(error) => {
                failures += 1;
                if !error.is_transient() || failures >= policy.attempts.max(1) {
                    return Err(error);
                }
                let Some(wait) = wait_before_retry(policy, failures, &error) else {
                    return Err(error);
                };
                tokio::time::sleep(wait).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;
    use crate::error::ErrorKind;

    fn run<T>(future: impl Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap()
            .block_on(future)
    }

    fn fast() -> Policy {
        Policy {
            attempts: 3,
            pauses: vec![Duration::ZERO],
        }
    }

    fn failing(
        kind: ErrorKind,
        then_ok_after: u32,
    ) -> (Cell<u32>, impl Fn(&Cell<u32>) -> Result<u32, ProviderError>) {
        (Cell::new(0), move |calls: &Cell<u32>| {
            calls.set(calls.get() + 1);
            if calls.get() > then_ok_after {
                Ok(calls.get())
            } else {
                Err(ProviderError::new(kind, "x"))
            }
        })
    }

    #[test]
    fn a_transient_failure_is_tried_again_until_it_works() {
        let (calls, step) = failing(ErrorKind::Server, 2);
        let result = run(retry(&fast(), || {
            let r = step(&calls);
            async move { r }
        }));
        assert_eq!(result, Ok(3));
        assert_eq!(calls.get(), 3);
    }

    #[test]
    fn the_tries_run_out() {
        let (calls, step) = failing(ErrorKind::Timeout, 99);
        let result = run(retry(&fast(), || {
            let r = step(&calls);
            async move { r }
        }));
        assert_eq!(result.unwrap_err().kind, ErrorKind::Timeout);
        assert_eq!(calls.get(), 3, "three tries in all");
    }

    #[test]
    fn failures_that_cannot_pass_are_not_tried_again() {
        for kind in [
            ErrorKind::Auth,
            ErrorKind::Cancelled,
            ErrorKind::KeyNotAllowed,
            ErrorKind::NotFound,
            ErrorKind::Offline,
            ErrorKind::BadRequest,
        ] {
            let (calls, step) = failing(kind, 99);
            let result = run(retry(&fast(), || {
                let r = step(&calls);
                async move { r }
            }));
            assert_eq!(result.unwrap_err().kind, kind);
            assert_eq!(calls.get(), 1, "{kind:?} is tried once");
        }
    }

    fn rate_limited(wait: Option<Duration>) -> ProviderError {
        ProviderError::new(ErrorKind::RateLimited, "HTTP 429").with_retry_after(wait)
    }

    #[test]
    fn a_retry_after_longer_than_the_pause_is_waited_out() {
        let calls = Cell::new(0);
        let start = std::time::Instant::now();
        let result = run(retry(&fast(), || {
            calls.set(calls.get() + 1);
            let r = if calls.get() < 2 {
                Err(rate_limited(Some(Duration::from_millis(120))))
            } else {
                Ok(calls.get())
            };
            async move { r }
        }));
        assert_eq!(result, Ok(2));
        // The policy's own pause is zero here, so the whole wait is the server's.
        assert!(
            start.elapsed() >= Duration::from_millis(120),
            "{:?}",
            start.elapsed()
        );
    }

    #[test]
    fn a_retry_after_shorter_than_the_pause_does_not_shorten_it() {
        let slow = Policy {
            attempts: 2,
            pauses: vec![Duration::from_millis(120)],
        };
        let calls = Cell::new(0);
        let start = std::time::Instant::now();
        let _ = run(retry(&slow, || {
            calls.set(calls.get() + 1);
            let r = if calls.get() < 2 {
                Err(rate_limited(Some(Duration::from_millis(1))))
            } else {
                Ok(())
            };
            async move { r }
        }));
        assert!(start.elapsed() >= Duration::from_millis(120));
    }

    #[test]
    fn a_retry_after_over_the_limit_is_not_waited_for() {
        let calls = Cell::new(0);
        let start = std::time::Instant::now();
        let error = run(retry(&fast(), || {
            calls.set(calls.get() + 1);
            async { Err::<(), _>(rate_limited(Some(LONGEST_WAIT + Duration::from_secs(1)))) }
        }))
        .unwrap_err();
        assert_eq!(calls.get(), 1, "no second try");
        assert!(start.elapsed() < Duration::from_secs(2));
        // The error still carries the wait, so the line can say it.
        assert_eq!(
            error.retry_after,
            Some(LONGEST_WAIT + Duration::from_secs(1))
        );
        // Exactly the limit is still waited for (a zero pause here keeps the test quick, so
        // only the decision is looked at: the second try happens).
        let calls = Cell::new(0);
        let _ = run(retry(
            &Policy {
                attempts: 2,
                pauses: vec![Duration::ZERO],
            },
            || {
                calls.set(calls.get() + 1);
                async { Err::<(), _>(rate_limited(Some(Duration::from_millis(1)))) }
            },
        ));
        assert_eq!(calls.get(), 2);
    }

    #[test]
    fn the_once_policy_never_repeats() {
        let (calls, step) = failing(ErrorKind::Server, 99);
        let _ = run(retry(&Policy::once(), || {
            let r = step(&calls);
            async move { r }
        }));
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn the_pauses_grow_then_repeat_the_last() {
        let policy = Policy::standard();
        assert_eq!(policy.pause(1), Duration::from_millis(500));
        assert_eq!(policy.pause(2), Duration::from_secs(1));
        assert_eq!(policy.pause(7), Duration::from_secs(1));
        assert_eq!(Policy::once().pause(1), Duration::ZERO);
    }
}
