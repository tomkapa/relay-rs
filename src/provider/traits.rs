use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;

use super::chat::{ChatRequest, ChatResponse};
use super::error::ProviderError;

/// The single seam between the agent and any LLM backend.
///
/// Implementations:
/// * MUST translate provider-specific errors into [`ProviderError`] variants.
/// * MUST NOT panic on malformed responses — return [`ProviderError::Decode`] instead.
/// * SHOULD respect cancellation by returning promptly when the future is dropped (the
///   agent layer wraps every call in `tokio::time::timeout`).
#[async_trait]
pub trait LlmProvider: Send + Sync {
    /// Identifier used in tracing fields (`relay.provider`). Low-cardinality, stable.
    fn name(&self) -> &'static str;

    async fn send(&self, request: ChatRequest) -> Result<ChatResponse, ProviderError>;
}

/// Reference-counted handle so the agent can clone cheaply without taking a generic.
#[derive(Clone)]
pub struct SharedProvider(Arc<dyn LlmProvider>);

impl SharedProvider {
    #[must_use]
    pub fn new(provider: Arc<dyn LlmProvider>) -> Self {
        Self(provider)
    }

    #[must_use]
    pub fn name(&self) -> &'static str {
        self.0.name()
    }

    pub async fn send(&self, request: ChatRequest) -> Result<ChatResponse, ProviderError> {
        self.0.send(request).await
    }
}

impl fmt::Debug for SharedProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SharedProvider")
            .field("name", &self.0.name())
            .finish()
    }
}
