use std::future::Future;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

use clawnicle_core::{Error, LlmRequest, LlmResponse, Result};

use crate::provider::LlmProvider;

/// A queue-backed fake provider for tests. Returns canned responses in order;
/// tracks how many times `complete` was invoked (so tests can assert the
/// journal cache actually short-circuits).
///
/// If the queue is exhausted, `complete` returns `Error::Tool("mock provider exhausted")`.
pub struct MockProvider {
    queue: Mutex<Vec<LlmResponse>>,
    calls: AtomicU32,
}

impl MockProvider {
    pub fn new(responses: Vec<LlmResponse>) -> Self {
        Self {
            queue: Mutex::new(responses.into_iter().rev().collect()),
            calls: AtomicU32::new(0),
        }
    }

    pub fn call_count(&self) -> u32 {
        self.calls.load(Ordering::SeqCst)
    }
}

impl LlmProvider for MockProvider {
    fn complete<'a>(
        &'a self,
        _request: &'a LlmRequest,
    ) -> impl Future<Output = Result<LlmResponse>> + Send + 'a {
        async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.queue
                .lock()
                .expect("mock queue mutex poisoned")
                .pop()
                .ok_or_else(|| Error::Tool("mock provider exhausted".into()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clawnicle_core::{LlmMessage, LlmRequest};

    #[tokio::test]
    async fn mock_returns_queued_responses_in_order() {
        let provider = MockProvider::new(vec![
            LlmResponse {
                model: "m".into(),
                content: "first".into(),
                tokens_in: 1,
                tokens_out: 1,
            },
            LlmResponse {
                model: "m".into(),
                content: "second".into(),
                tokens_in: 1,
                tokens_out: 1,
            },
        ]);
        let req = LlmRequest::new("m", vec![LlmMessage::user("hi")]);
        assert_eq!(provider.complete(&req).await.unwrap().content, "first");
        assert_eq!(provider.complete(&req).await.unwrap().content, "second");
        assert!(provider.complete(&req).await.is_err());
        assert_eq!(provider.call_count(), 3);
    }
}
