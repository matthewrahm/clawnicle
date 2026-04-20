use std::time::Duration;

/// Retry policy applied to a tool call.
///
/// `max_attempts = 1` means no retry (try once, fail or succeed). `>=2` enables
/// retry with exponential backoff. `timeout` is applied per attempt; the total
/// wallclock can exceed it by up to `(max_attempts - 1) * max_backoff`.
#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub initial_backoff: Duration,
    pub max_backoff: Duration,
    pub backoff_multiplier: f32,
    pub timeout: Option<Duration>,
}

impl RetryPolicy {
    /// Try once, no retry. The default.
    pub const fn none() -> Self {
        Self {
            max_attempts: 1,
            initial_backoff: Duration::from_millis(0),
            max_backoff: Duration::from_millis(0),
            backoff_multiplier: 1.0,
            timeout: None,
        }
    }

    /// Three attempts with exponential backoff starting at 100ms, capped at 5s.
    /// A sensible default for flaky external APIs.
    pub const fn exponential_3() -> Self {
        Self {
            max_attempts: 3,
            initial_backoff: Duration::from_millis(100),
            max_backoff: Duration::from_secs(5),
            backoff_multiplier: 2.0,
            timeout: None,
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    pub fn with_max_attempts(mut self, max_attempts: u32) -> Self {
        self.max_attempts = max_attempts;
        self
    }
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self::none()
    }
}
