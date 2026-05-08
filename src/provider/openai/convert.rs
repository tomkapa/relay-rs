//! Bidirectional conversion between provider-agnostic chat types and the wire schema we
//! send to OpenAI-Chat-Completions-compatible endpoints.
//!
//! We don't use `async_openai::types::CreateChatCompletionRequest` directly because some
//! compatible endpoints (DeepSeek V4 thinking-mode models) require the assistant's
//! `reasoning_content` to be replayed alongside `tool_calls` on subsequent turns
//! (api-docs.deepseek.com/guides/thinking_mode). The stock OpenAI request schema has no
//! such field, so we define our own typed request body that carries it. Stock OpenAI
//! ignores unknown fields, so the same payload works against both.

use async_openai::types::chat::{
    ChatCompletionMessageToolCall, ChatCompletionMessageToolCalls, ChatCompletionTool,
    ChatCompletionTools, FinishReason, FunctionCall, FunctionObject,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::provider::chat::{
    AssistantContent, ChatMessage, StopReason, ToolCall, ToolCallId, ToolResult, ToolSpec,
    UserContent,
};
use crate::types::ToolName;

/// Top-level chat-completion request body.
#[derive(Debug, Serialize)]
pub(super) struct ChatRequestBody {
    pub model: String,
    pub messages: Vec<WireMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ChatCompletionTools>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_completion_tokens: Option<u32>,
}

/// One message on the wire. `role` is the discriminant; only the variants we actually
/// emit are spelled out, so adding a new role (e.g. developer) is a deliberate choice.
#[derive(Debug, Serialize)]
#[serde(tag = "role", rename_all = "lowercase")]
pub(super) enum WireMessage {
    System {
        content: String,
    },
    User {
        content: String,
    },
    Assistant {
        #[serde(skip_serializing_if = "Option::is_none")]
        content: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        tool_calls: Option<Vec<ChatCompletionMessageToolCalls>>,
        /// DeepSeek V4 thinking-mode extension. When the prior assistant turn contained
        /// `tool_calls`, this field MUST be replayed verbatim on the next request or the
        /// API rejects with an `invalid_request_error`. Stock OpenAI tolerates the
        /// unknown field.
        #[serde(skip_serializing_if = "Option::is_none")]
        reasoning_content: Option<String>,
    },
    Tool {
        content: String,
        tool_call_id: String,
    },
}

/// Top-level chat-completion response body. We deserialize only the fields the agent
/// loop consumes so that future provider extensions don't break parsing.
#[derive(Debug, Deserialize)]
pub(super) struct ChatResponseBody {
    pub choices: Vec<WireChoice>,
    /// Model id reported by the provider — may differ from the request when a
    /// gateway routes to a specific snapshot. `default` because some
    /// OpenAI-compatible endpoints omit it.
    #[serde(default)]
    pub model: Option<String>,
    /// Token-usage counts. `default` because some endpoints omit the field on
    /// streaming responses; we do not stream today, but the safety net is cheap.
    #[serde(default)]
    pub usage: Option<WireUsage>,
}

/// Token-usage counters returned by an OpenAI-Chat-Completions endpoint. Field
/// names match the wire shape; we map to the provider-agnostic
/// [`crate::provider::chat::Usage`] in [`super::client`].
#[derive(Debug, Deserialize)]
pub(super) struct WireUsage {
    #[serde(default)]
    pub prompt_tokens: u32,
    #[serde(default)]
    pub completion_tokens: u32,
}

#[derive(Debug, Deserialize)]
pub(super) struct WireChoice {
    #[serde(default)]
    pub finish_reason: Option<FinishReason>,
    pub message: WireResponseMessage,
}

#[derive(Debug, Deserialize)]
pub(super) struct WireResponseMessage {
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub reasoning_content: Option<String>,
    #[serde(default)]
    pub tool_calls: Option<Vec<ChatCompletionMessageToolCalls>>,
}

/// Build the leading `role=system` message from the request's system prompt.
pub(super) fn system_message(prompt: &str) -> WireMessage {
    WireMessage::System {
        content: prompt.to_string(),
    }
}

/// Translate one provider-agnostic message into one or more wire messages.
pub(super) fn message_to_wire(msg: ChatMessage) -> Vec<WireMessage> {
    match msg {
        ChatMessage::User(content) => user_to_wire(content),
        ChatMessage::Assistant(content) => assistant_to_wire(content),
    }
}

/// A user turn may carry text *and* tool results. OpenAI represents these as separate
/// messages (`role=user` for text, `role=tool` for each result), so we emit one per block
/// in source order. Order matters: tool results have to follow the assistant `tool_calls`
/// they answer, and same-turn text comes after.
fn user_to_wire(blocks: Vec<UserContent>) -> Vec<WireMessage> {
    let mut out: Vec<WireMessage> = Vec::with_capacity(blocks.len());
    for block in blocks {
        match block {
            UserContent::Text(t) => out.push(WireMessage::User { content: t }),
            UserContent::ToolResult(ToolResult {
                call_id,
                output,
                is_error,
            }) => {
                // OpenAI has no `is_error` flag on tool messages; the convention is to
                // surface failures as plain text in `content`. Caller has already done
                // that; the bool is a no-op on this provider.
                let _ = is_error;
                out.push(WireMessage::Tool {
                    content: output,
                    tool_call_id: call_id.as_str().to_string(),
                });
            }
        }
    }
    out
}

/// An assistant turn collapses into a single wire message that carries any combination of
/// `content` (concatenated text), `tool_calls`, and `reasoning_content`.
fn assistant_to_wire(blocks: Vec<AssistantContent>) -> Vec<WireMessage> {
    let mut text = String::new();
    let mut reasoning = String::new();
    let mut tool_calls: Vec<ChatCompletionMessageToolCalls> = Vec::new();
    for block in blocks {
        match block {
            AssistantContent::Text(t) => {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(&t);
            }
            AssistantContent::Reasoning(r) => {
                if !reasoning.is_empty() {
                    reasoning.push('\n');
                }
                reasoning.push_str(&r);
            }
            AssistantContent::ToolCall(call) => {
                tool_calls.push(ChatCompletionMessageToolCalls::Function(
                    ChatCompletionMessageToolCall {
                        id: call.id.as_str().to_string(),
                        function: FunctionCall {
                            name: call.name.as_str().to_string(),
                            arguments: call.input.to_string(),
                        },
                    },
                ));
            }
        }
    }

    // `bool::then_some` is the one-liner equivalent of
    // `if cond { Some(val) } else { None }`. The string/vec is moved into the call in
    // either branch — same semantics as the if-else, fewer lines.
    let content = (!text.is_empty()).then_some(text);
    let tool_calls = (!tool_calls.is_empty()).then_some(tool_calls);
    let reasoning_content = (!reasoning.is_empty()).then_some(reasoning);

    vec![WireMessage::Assistant {
        content,
        tool_calls,
        reasoning_content,
    }]
}

/// Map a tool spec to OpenAI's function-tool envelope. The `parameters` field is the
/// JSON-schema body verbatim — the registry already validated it.
pub(super) fn tool_spec_to_wire(spec: &ToolSpec) -> ChatCompletionTools {
    ChatCompletionTools::Function(ChatCompletionTool {
        function: FunctionObject {
            name: spec.name.as_str().to_string(),
            description: Some(spec.description.to_string()),
            parameters: Some((*spec.input_schema).clone()),
            strict: None,
        },
    })
}

/// Lift one choice from a response into our content vec. Skips tool calls whose id or
/// name we cannot parse — that means the upstream sent something we wouldn't have
/// registered, and the agent loop terminates cleanly rather than looping on garbage.
pub(super) fn choice_to_content(choice: WireChoice) -> Vec<AssistantContent> {
    let mut out: Vec<AssistantContent> = Vec::new();
    let msg = choice.message;

    // Reasoning first so it sits in front of the visible text in the session — matches
    // the order the model "thought, then spoke" and keeps replay deterministic.
    if let Some(r) = msg.reasoning_content
        && !r.is_empty()
    {
        out.push(AssistantContent::Reasoning(r));
    }

    if let Some(text) = msg.content
        && !text.is_empty()
    {
        out.push(AssistantContent::Text(text));
    }

    if let Some(calls) = msg.tool_calls {
        for call in calls {
            // The Custom variant is for OpenAI's experimental free-form tools; we don't
            // emit them and don't replay them.
            let ChatCompletionMessageToolCalls::Function(fc) = call else {
                continue;
            };
            let Ok(id) = ToolCallId::try_from(fc.id.as_str()) else {
                continue;
            };
            let Ok(name) = ToolName::try_from(fc.function.name.as_str()) else {
                continue;
            };
            // Arguments come back as a JSON-encoded string. If parsing fails the model
            // produced malformed JSON — surface as `Null` so the tool's own schema
            // validation rejects it with a clear error.
            let input = serde_json::from_str(&fc.function.arguments).unwrap_or(Value::Null);
            out.push(AssistantContent::ToolCall(ToolCall { id, name, input }));
        }
    }

    out
}

pub(super) fn map_finish_reason(reason: Option<FinishReason>) -> StopReason {
    match reason {
        Some(FinishReason::Stop) | None => StopReason::EndTurn,
        Some(FinishReason::ToolCalls | FinishReason::FunctionCall) => StopReason::ToolUse,
        Some(FinishReason::Length) => StopReason::MaxTokens,
        Some(FinishReason::ContentFilter) => StopReason::Other("content_filter".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::chat::{ToolCall, ToolCallId, ToolResult};
    use crate::types::ToolName;
    use serde_json::json;

    fn tool_id(s: &str) -> ToolCallId {
        ToolCallId::try_from(s).expect("valid id")
    }

    #[test]
    fn user_text_becomes_one_user_message() {
        let wire = message_to_wire(ChatMessage::User(vec![UserContent::Text("hi".into())]));
        assert_eq!(wire.len(), 1);
        assert!(matches!(wire[0], WireMessage::User { .. }));
    }

    #[test]
    fn user_tool_results_split_into_tool_messages() {
        let wire = message_to_wire(ChatMessage::User(vec![
            UserContent::ToolResult(ToolResult {
                call_id: tool_id("c1"),
                output: "ok".into(),
                is_error: false,
            }),
            UserContent::ToolResult(ToolResult {
                call_id: tool_id("c2"),
                output: "boom".into(),
                is_error: true,
            }),
        ]));
        assert_eq!(wire.len(), 2);
        for m in &wire {
            assert!(matches!(m, WireMessage::Tool { .. }));
        }
    }

    #[test]
    fn assistant_text_and_tool_calls_collapse_to_one_message() {
        let wire = message_to_wire(ChatMessage::Assistant(vec![
            AssistantContent::Text("calling".into()),
            AssistantContent::ToolCall(ToolCall {
                id: tool_id("tc1"),
                name: ToolName::try_from("search").expect("valid"),
                input: json!({"q": "rust"}),
            }),
        ]));
        assert_eq!(wire.len(), 1);
        let WireMessage::Assistant {
            content,
            tool_calls,
            reasoning_content,
        } = &wire[0]
        else {
            panic!("expected assistant");
        };
        assert!(content.is_some());
        assert_eq!(tool_calls.as_ref().map(Vec::len), Some(1));
        assert!(reasoning_content.is_none());
    }

    #[test]
    fn assistant_reasoning_replayed_alongside_tool_calls() {
        let wire = message_to_wire(ChatMessage::Assistant(vec![
            AssistantContent::Reasoning("thinking step 1".into()),
            AssistantContent::ToolCall(ToolCall {
                id: tool_id("tc1"),
                name: ToolName::try_from("search").expect("valid"),
                input: json!({}),
            }),
        ]));
        let WireMessage::Assistant {
            reasoning_content, ..
        } = &wire[0]
        else {
            panic!("expected assistant");
        };
        assert_eq!(reasoning_content.as_deref(), Some("thinking step 1"));
    }

    #[test]
    fn assistant_reasoning_only_emits_reasoning_content() {
        let wire = message_to_wire(ChatMessage::Assistant(vec![AssistantContent::Reasoning(
            "secret".into(),
        )]));
        let WireMessage::Assistant {
            content,
            tool_calls,
            reasoning_content,
        } = &wire[0]
        else {
            panic!("expected assistant");
        };
        assert!(content.is_none());
        assert!(tool_calls.is_none());
        assert_eq!(reasoning_content.as_deref(), Some("secret"));
    }

    #[test]
    fn assistant_with_no_content_serializes_without_optional_fields() {
        // Defensive: an assistant turn with neither content, tool_calls, nor reasoning
        // (shouldn't happen but worth pinning) serializes to `{"role":"assistant"}`,
        // not to a body with explicit nulls that some providers reject.
        let wire = WireMessage::Assistant {
            content: None,
            tool_calls: None,
            reasoning_content: None,
        };
        let json = serde_json::to_string(&wire).expect("serializes");
        assert_eq!(json, r#"{"role":"assistant"}"#);
    }
}
