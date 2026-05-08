use async_openai::Client;
use async_openai::config::OpenAIConfig;
use async_openai::error::OpenAIError;
use async_trait::async_trait;
use tracing::instrument;

use super::convert::{
    ChatRequestBody, ChatResponseBody, choice_to_content, map_finish_reason, message_to_wire,
    system_message, tool_spec_to_wire,
};
use crate::observability::gen_ai;
use crate::provider::chat::{ChatRequest, ChatResponse, Usage};
use crate::provider::error::ProviderError;
use crate::provider::traits::LlmProvider;
use crate::types::{ModelId, SecretString};

/// OpenAI-Chat-Completions implementation of [`LlmProvider`].
pub struct OpenAiProvider {
    client: Client<OpenAIConfig>,
}

impl std::fmt::Debug for OpenAiProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAiProvider").finish_non_exhaustive()
    }
}

impl OpenAiProvider {
    /// Construct a provider against an OpenAI-Chat-Completions-compatible endpoint.
    /// Pass `base_url` to point at DeepSeek (`https://api.deepseek.com/v1`), Together,
    /// Groq, or any in-house gateway. Omit it to hit the public OpenAI API.
    pub fn new(api_key: &SecretString, base_url: Option<String>) -> Self {
        // §6: assert the boundary precondition the type system already proves, so a
        // future refactor that loosens `SecretString` does not silently let an empty key
        // through.
        assert!(!api_key.is_empty(), "SecretString invariant: non-empty");

        let mut config = OpenAIConfig::new().with_api_key(api_key.expose());
        if let Some(url) = base_url {
            config = config.with_api_base(url);
        }
        Self {
            client: Client::with_config(config),
        }
    }
}

#[async_trait]
impl LlmProvider for OpenAiProvider {
    fn name(&self) -> &'static str {
        "openai"
    }

    // GenAI semconv fields are declared `Empty` here and recorded inside the body so
    // both the request- and response-shaped attributes ride on the same span. Keeping
    // the span name stable (`provider.openai.send`) per CLAUDE.md §2; the spec's
    // recommended `chat <model>` form would put the model in the name and inflate
    // cardinality.
    #[instrument(
        name = "provider.openai.send",
        skip_all,
        fields(
            relay.provider = "openai",
            relay.model = %request.model,
            relay.messages = request.messages.len(),
            relay.tools = request.tools.len(),
            relay.max_output_tokens = request.max_output_tokens.get(),
            gen_ai.system = tracing::field::Empty,
            gen_ai.operation.name = tracing::field::Empty,
            gen_ai.request.model = tracing::field::Empty,
            gen_ai.request.max_tokens = tracing::field::Empty,
            gen_ai.response.model = tracing::field::Empty,
            gen_ai.response.finish_reasons = tracing::field::Empty,
            gen_ai.usage.input_tokens = tracing::field::Empty,
            gen_ai.usage.output_tokens = tracing::field::Empty,
            gen_ai.input.messages = tracing::field::Empty,
            gen_ai.output.messages = tracing::field::Empty,
        ),
    )]
    async fn send(&self, request: ChatRequest) -> Result<ChatResponse, ProviderError> {
        gen_ai::record_chat_request("openai", &request);

        let mut messages = Vec::with_capacity(request.messages.len() + 1);
        messages.push(system_message(&request.system));
        for msg in request.messages {
            messages.extend(message_to_wire(msg));
        }

        // `.then(|| …)` defers the `.collect()` to the non-empty case so we don't
        // allocate an empty `Vec` only to throw it away.
        let tools = (!request.tools.is_empty())
            .then(|| request.tools.iter().map(tool_spec_to_wire).collect());

        let body = ChatRequestBody {
            model: request.model.as_str().to_string(),
            messages,
            tools,
            max_completion_tokens: Some(request.max_output_tokens.get()),
        };

        // Use the BYOT (bring-your-own-type) path so we can send `reasoning_content` on
        // assistant messages and read it back on responses — fields the stock
        // async-openai schema doesn't model. See `convert.rs` for why this matters.
        let response: ChatResponseBody = self
            .client
            .chat()
            .create_byot(body)
            .await
            .map_err(map_runtime_error)?;

        // Extract usage / model before consuming `response.choices` below — these
        // attributes ride on the span, not on the request shape.
        let usage = response
            .usage
            .as_ref()
            .map(|u| Usage {
                input_tokens: u.prompt_tokens,
                output_tokens: u.completion_tokens,
                cache_creation_input_tokens: None,
                cache_read_input_tokens: None,
            })
            .unwrap_or_default();
        let model = response
            .model
            .as_deref()
            .and_then(|m| ModelId::try_from(m).ok());

        let choice = response
            .choices
            .into_iter()
            .next()
            .ok_or(ProviderError::EmptyResponse)?;
        let stop_reason = map_finish_reason(choice.finish_reason);
        let content = choice_to_content(choice);
        if content.is_empty() {
            return Err(ProviderError::EmptyResponse);
        }

        let resp = ChatResponse {
            content,
            stop_reason,
            usage,
            model,
        };
        gen_ai::record_chat_response(&resp);
        Ok(resp)
    }
}

/// Map `async-openai`'s error onto our `ProviderError` variants. We branch on the API
/// error code/type when present so the agent's retry layer can act on the right
/// discriminant; only the `Transport` / `Transient` strings are free-form.
fn map_runtime_error(err: OpenAIError) -> ProviderError {
    match err {
        OpenAIError::ApiError(api) => match api.code.as_deref() {
            Some("invalid_api_key" | "authentication_error") => ProviderError::Unauthorized,
            Some("rate_limit_exceeded" | "insufficient_quota") => ProviderError::RateLimited,
            _ => match api.r#type.as_deref() {
                Some("authentication_error") => ProviderError::Unauthorized,
                Some("rate_limit_error") => ProviderError::RateLimited,
                Some("server_error" | "api_error") => ProviderError::Transient(api.to_string()),
                _ => ProviderError::InvalidRequest(api.to_string()),
            },
        },
        OpenAIError::Reqwest(e) if e.is_timeout() || e.is_connect() => {
            ProviderError::Transient(e.to_string())
        }
        OpenAIError::Reqwest(e) => ProviderError::Transport(e.to_string()),
        OpenAIError::JSONDeserialize(e, _) => ProviderError::Decode(e.to_string()),
        OpenAIError::StreamError(e) => ProviderError::Transport(e.to_string()),
        OpenAIError::FileSaveError(s) | OpenAIError::FileReadError(s) => {
            ProviderError::Transport(s)
        }
        OpenAIError::InvalidArgument(s) => ProviderError::InvalidRequest(s),
    }
}
