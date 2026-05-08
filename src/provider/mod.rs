//! Provider-agnostic LLM interface.
//!
//! The `Agent` talks to providers exclusively through [`LlmProvider`] and the chat types
//! defined in [`chat`]. Adding a new backend (OpenAI, Ollama, local, mock) is a matter of
//! implementing the trait — `Agent::reply` does not change.

pub mod anthropic;
mod chat;
mod error;
pub mod openai;
mod traits;

pub use chat::{
    AssistantContent, ChatMessage, ChatRequest, ChatResponse, Role, StopReason,
    TOOL_CALL_ID_MAX_BYTES, ToolCall, ToolCallId, ToolResult, ToolSpec, Usage, UserContent,
};
pub use error::ProviderError;
pub use traits::{LlmProvider, SharedProvider};
