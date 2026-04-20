use std::future::Future;

use clawnicle_core::{LlmRequest, LlmResponse, Result};

/// A language-model completion backend. Implementations talk to a real API
/// (Anthropic, OpenAI) or return canned responses (MockProvider for tests).
///
/// The runtime wraps every call through a provider in a journal cache, so
/// providers themselves are free to be stateless and non-deterministic — the
/// caching happens one layer above.
pub trait LlmProvider: Send + Sync {
    fn complete<'a>(
        &'a self,
        request: &'a LlmRequest,
    ) -> impl Future<Output = Result<LlmResponse>> + Send + 'a;
}
