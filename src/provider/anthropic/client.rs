use async_trait::async_trait;
use claudius::{Anthropic, MessageCreateParams};
use tracing::instrument;

use super::convert::{
    block_to_assistant, map_stop_reason, message_to_param, parse_model, tool_spec_to_param,
};
use crate::provider::chat::{ChatRequest, ChatResponse};
use crate::provider::error::ProviderError;
use crate::provider::traits::LlmProvider;
use crate::types::SecretString;

/// Anthropic-Messages-API implementation of [`LlmProvider`].
pub struct AnthropicProvider {
    client: Anthropic,
}

impl std::fmt::Debug for AnthropicProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnthropicProvider").finish_non_exhaustive()
    }
}

impl AnthropicProvider {
    /// Construct a provider against the public API. Pass `base_url` to point at a proxy
    /// or compatible endpoint.
    pub fn new(api_key: &SecretString, base_url: Option<String>) -> Result<Self, ProviderError> {
        // §6: assert the boundary precondition the type system already proves, so a future
        // refactor that loosens `SecretString` does not silently let an empty key through.
        assert!(!api_key.is_empty(), "SecretString invariant: non-empty");

        let mut client =
            Anthropic::new(Some(api_key.expose().to_string())).map_err(map_construction_error)?;
        if let Some(url) = base_url {
            client = client.with_base_url(url);
        }
        Ok(Self { client })
    }
}

#[async_trait]
impl LlmProvider for AnthropicProvider {
    fn name(&self) -> &'static str {
        "anthropic"
    }

    #[instrument(
        name = "provider.anthropic.send",
        skip_all,
        fields(
            relay.provider = "anthropic",
            relay.model = %request.model,
            relay.messages = request.messages.len(),
            relay.tools = request.tools.len(),
            relay.max_output_tokens = request.max_output_tokens.get(),
        ),
    )]
    async fn send(&self, request: ChatRequest) -> Result<ChatResponse, ProviderError> {
        let model = parse_model(request.model.as_str());

        // `.then(|| …)` defers the `.collect()` to the non-empty case so we don't
        // allocate an empty `Vec` only to throw it away.
        let tools = (!request.tools.is_empty())
            .then(|| request.tools.iter().map(tool_spec_to_param).collect());

        let messages = request.messages.into_iter().map(message_to_param).collect();

        let mut params = MessageCreateParams::new(request.max_output_tokens.get(), messages, model)
            .with_system_string(request.system.to_string());
        if let Some(tools) = tools {
            params = params.with_tools(tools);
        }

        let response = self.client.send(params).await.map_err(map_runtime_error)?;

        let content: Vec<_> = response
            .content
            .into_iter()
            .filter_map(block_to_assistant)
            .collect();
        if content.is_empty() {
            return Err(ProviderError::EmptyResponse);
        }

        Ok(ChatResponse {
            content,
            stop_reason: map_stop_reason(response.stop_reason),
        })
    }
}

/// Errors raised during client construction (bad URL, missing key etc.).
fn map_construction_error(err: claudius::Error) -> ProviderError {
    if err.is_authentication() {
        return ProviderError::Unauthorized;
    }
    ProviderError::InvalidRequest(err.to_string())
}

/// Errors raised by `send` — surface the most actionable variant. Callers (the agent
/// retry/timeout layer) only branch on the `ProviderError` discriminant, never the
/// underlying string.
fn map_runtime_error(err: claudius::Error) -> ProviderError {
    if err.is_authentication() {
        return ProviderError::Unauthorized;
    }
    if err.is_rate_limit() {
        return ProviderError::RateLimited;
    }
    if err.is_timeout() || err.is_connection() || err.is_server_error() {
        return ProviderError::Transient(err.to_string());
    }
    if err.is_bad_request() {
        return ProviderError::InvalidRequest(err.to_string());
    }
    ProviderError::Transport(err.to_string())
}
