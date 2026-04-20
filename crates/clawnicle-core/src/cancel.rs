use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Cooperative cancellation handle. Cloneable; all clones share one flag.
///
/// Workflow authors pass a CancelToken into a [`Context`] via
/// `Context::with_cancel_token`; external callers (scheduler, CLI, signal
/// handler) flip the flag by calling [`CancelToken::cancel`]. The runtime
/// checks the flag at natural suspension points — specifically before each
/// tool-call attempt — and returns `Error::Cancelled` when set.
///
/// Cancellation is cooperative: user closures themselves are not interrupted
/// mid-await. If a closure is stuck, a per-attempt timeout (via
/// [`crate::RetryPolicy::with_timeout`]) is the right escape hatch.
#[derive(Debug, Clone, Default)]
pub struct CancelToken {
    inner: Arc<AtomicBool>,
}

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.inner.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.inner.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_clones_share_one_flag() {
        let a = CancelToken::new();
        let b = a.clone();
        assert!(!a.is_cancelled());
        b.cancel();
        assert!(a.is_cancelled());
    }
}
