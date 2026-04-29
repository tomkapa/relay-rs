//! Provider-agnostic chat data model.
//!
//! Asymmetry between user and assistant turns is modelled directly: a user message can
//! contain text or tool results; an assistant message can contain text, reasoning, or tool
//! calls. This stops impossible states (e.g. assistant emitting a `ToolResult`) at the
//! type level instead of relying on runtime checks.

use std::sync::Arc;

use serde_json::Value;

use crate::types::{MaxOutputTokens, ModelId, ToolName};

/// Identifier connecting an assistant `ToolCall` to its corresponding user `ToolResult`.
///
/// Providers generate these and we round-trip them verbatim — never invent or rewrite.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ToolCallId(pub Arc<str>);

impl ToolCallId {
    #[must_use]
    pub fn new(id: impl Into<Arc<str>>) -> Self {
        Self(id.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A single message in the conversation, distinguished by speaker. Each role permits a
/// different set of content blocks — the enum encodes that asymmetry.
#[derive(Debug, Clone)]
pub enum ChatMessage {
    User(Vec<UserContent>),
    Assistant(Vec<AssistantContent>),
}

/// Content allowed in a user-role message.
#[derive(Debug, Clone)]
pub enum UserContent {
    Text(String),
    ToolResult(ToolResult),
}

/// Content allowed in an assistant-role message.
#[derive(Debug, Clone)]
pub enum AssistantContent {
    Text(String),
    /// Reasoning / thinking blocks — opaque to us, round-tripped to providers that
    /// support extended thinking. Logged at DEBUG only (PII per CLAUDE.md §2).
    Reasoning(String),
    ToolCall(ToolCall),
}

/// A model-issued request to invoke a tool.
#[derive(Debug, Clone)]
pub struct ToolCall {
    pub id: ToolCallId,
    pub name: ToolName,
    pub input: Value,
}

/// The result of running a tool, threaded back to the model in the next turn.
#[derive(Debug, Clone)]
pub struct ToolResult {
    pub call_id: ToolCallId,
    pub output: String,
    pub is_error: bool,
}

/// JSON-schema description of a tool exposed to a provider.
#[derive(Debug, Clone)]
pub struct ToolSpec {
    pub name: ToolName,
    pub description: Arc<str>,
    pub input_schema: Arc<Value>,
}

/// A complete chat request to a provider. Ownership is transferred — the provider may
/// drain or transform internally.
#[derive(Debug, Clone)]
pub struct ChatRequest {
    pub model: ModelId,
    pub system: Arc<str>,
    pub messages: Vec<ChatMessage>,
    pub tools: Arc<[ToolSpec]>,
    pub max_output_tokens: MaxOutputTokens,
}

/// A provider's reply for a single turn.
#[derive(Debug, Clone)]
pub struct ChatResponse {
    pub content: Vec<AssistantContent>,
    pub stop_reason: StopReason,
}

/// Why the provider stopped generating. Mapped from provider-specific signals.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopReason {
    /// Model finished its turn naturally.
    EndTurn,
    /// Model is requesting tool execution.
    ToolUse,
    /// Output budget exhausted.
    MaxTokens,
    /// Anything else the provider reports — kept as a string escape hatch so we don't
    /// lose information when adding new providers.
    Other(String),
}

impl ChatResponse {
    /// All tool calls in the response, in emission order.
    #[must_use]
    pub fn tool_calls(&self) -> Vec<&ToolCall> {
        self.content
            .iter()
            .filter_map(|c| match c {
                AssistantContent::ToolCall(call) => Some(call),
                _ => None,
            })
            .collect()
    }

    /// Concatenate all `Text` blocks with single newlines between them.
    #[must_use]
    pub fn text(&self) -> String {
        let mut out = String::new();
        for c in &self.content {
            if let AssistantContent::Text(t) = c {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(t);
            }
        }
        out
    }
}
