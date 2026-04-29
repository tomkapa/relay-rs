//! Provider-agnostic chat data model.
//!
//! Asymmetry between user and assistant turns is modelled directly: a user message can
//! contain text or tool results; an assistant message can contain text, reasoning, or tool
//! calls. This stops impossible states (e.g. assistant emitting a `ToolResult`) at the
//! type level instead of relying on runtime checks.

use std::sync::Arc;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

use crate::types::{MaxOutputTokens, ModelId, ParseError, ToolName};

/// Maximum bytes accepted in a `ToolCallId`.
///
/// Anthropic ids are short opaque strings (~30 chars). 256 bytes leaves headroom for any
/// provider while bounding the size of an attribute that ends up on every span and every
/// error.
pub const TOOL_CALL_ID_MAX_BYTES: usize = 256;

/// Identifier connecting an assistant `ToolCall` to its corresponding user `ToolResult`.
///
/// Providers generate these and we round-trip them verbatim — never invent or rewrite.
/// Construction is fallible: an empty or oversize id desyncs the call/result pairing,
/// which is silent corruption, so we reject it at the boundary instead.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ToolCallId(Arc<str>);

impl ToolCallId {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<&str> for ToolCallId {
    type Error = ParseError;

    fn try_from(raw: &str) -> Result<Self, Self::Error> {
        if raw.is_empty() {
            return Err(ParseError::Empty {
                field: "tool_call_id",
            });
        }
        if raw.len() > TOOL_CALL_ID_MAX_BYTES {
            return Err(ParseError::TooLong {
                field: "tool_call_id",
                max: TOOL_CALL_ID_MAX_BYTES,
                got: raw.len(),
            });
        }
        Ok(Self(Arc::from(raw)))
    }
}

impl TryFrom<String> for ToolCallId {
    type Error = ParseError;

    fn try_from(raw: String) -> Result<Self, Self::Error> {
        Self::try_from(raw.as_str())
    }
}

impl Serialize for ToolCallId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for ToolCallId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Self::try_from(raw).map_err(serde::de::Error::custom)
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
#[derive(Debug, Clone, Serialize)]
pub struct ToolCall {
    pub id: ToolCallId,
    pub name: ToolName,
    pub input: Value,
}

/// The result of running a tool, threaded back to the model in the next turn.
#[derive(Debug, Clone, Serialize)]
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_call_id_rejects_empty() {
        assert!(ToolCallId::try_from("").is_err());
    }

    #[test]
    fn tool_call_id_rejects_oversize() {
        let big = "a".repeat(TOOL_CALL_ID_MAX_BYTES + 1);
        assert!(ToolCallId::try_from(big.as_str()).is_err());
    }

    #[test]
    fn tool_call_id_round_trips_verbatim() {
        let id = ToolCallId::try_from("toolu_01abc").expect("valid");
        assert_eq!(id.as_str(), "toolu_01abc");
    }
}
