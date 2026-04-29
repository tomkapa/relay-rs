use std::time::Duration;

use tokio::time::timeout;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, instrument, warn};

use crate::clock::SharedClock;
use crate::hook::{HookChain, HookDecision, ToolContext, TurnContext};
use crate::memory::SharedMemory;
use crate::provider::{
    AssistantContent, ChatMessage, ChatRequest, ChatResponse, SharedProvider, ToolCall, ToolCallId,
    ToolResult, UserContent,
};
use crate::session::{SessionId, SharedSessionStore};
use crate::tools::ToolRegistry;
use crate::types::{MaxOutputTokens, MaxTurns, ModelId, Prompt};

use super::error::AgentError;
use super::limits::MAX_TOOL_CALLS_PER_TURN;

/// The agent runtime. All collaborators live behind shared trait handles so the agent
/// is end-to-end testable with no network and so any one of them can be swapped without
/// touching this struct.
#[derive(Debug, Clone)]
pub struct Agent {
    provider: SharedProvider,
    sessions: SharedSessionStore,
    memory: SharedMemory,
    #[allow(dead_code)] // wired through builder; consumed by future scheduling work.
    clock: SharedClock,
    tools: ToolRegistry,
    hooks: HookChain,
    model: ModelId,
    max_output_tokens: MaxOutputTokens,
    max_turns: MaxTurns,
    provider_timeout: Duration,
    tool_timeout: Duration,
}

impl Agent {
    #[allow(clippy::too_many_arguments)] // constructed via the builder; this is the seam.
    pub(super) fn new(
        provider: SharedProvider,
        sessions: SharedSessionStore,
        memory: SharedMemory,
        clock: SharedClock,
        tools: ToolRegistry,
        hooks: HookChain,
        model: ModelId,
        max_output_tokens: MaxOutputTokens,
        max_turns: MaxTurns,
        provider_timeout: Duration,
        tool_timeout: Duration,
    ) -> Self {
        Self {
            provider,
            sessions,
            memory,
            clock,
            tools,
            hooks,
            model,
            max_output_tokens,
            max_turns,
            provider_timeout,
            tool_timeout,
        }
    }

    /// Allocate a fresh session via the configured store. Returns the handle the caller
    /// passes to [`reply`](Self::reply) for subsequent turns.
    pub async fn start_session(&self) -> Result<SessionId, AgentError> {
        Ok(self.sessions.create().await?)
    }

    /// Drive one user prompt to a final assistant text answer, running tool calls in
    /// between turns. Honours `cancel`: if the token fires, returns
    /// [`AgentError::Cancelled`] at the next checkpoint.
    #[instrument(
        name = "agent.reply",
        skip_all,
        fields(
            relay.session.id = %session,
            relay.provider = self.provider.name(),
            relay.model = %self.model,
            relay.max_turns = self.max_turns.get(),
        ),
    )]
    pub async fn reply(
        &self,
        session: SessionId,
        prompt: Prompt,
        cancel: CancellationToken,
    ) -> Result<String, AgentError> {
        self.sessions
            .append(
                session,
                ChatMessage::User(vec![UserContent::Text(prompt.into_string())]),
            )
            .await?;

        for turn in 0..self.max_turns.get() {
            if cancel.is_cancelled() {
                return Err(AgentError::Cancelled);
            }

            let ctx = TurnContext {
                session_id: session,
                turn_index: turn,
            };
            self.guard(self.hooks.before_turn(ctx).await?)?;

            let response = self.send_one_turn(session, &cancel).await?;

            self.guard(self.hooks.after_turn(ctx, &response).await?)?;

            self.sessions
                .append(session, ChatMessage::Assistant(response.content.clone()))
                .await?;

            let tool_calls = response.tool_calls();
            if tool_calls.is_empty() {
                let text = response.text();
                if text.is_empty() {
                    return Err(AgentError::EmptyReply);
                }
                info!(turn, "agent.turn.final");
                return Ok(text);
            }
            if tool_calls.len() > MAX_TOOL_CALLS_PER_TURN {
                return Err(AgentError::TooManyToolCalls {
                    max: MAX_TOOL_CALLS_PER_TURN,
                });
            }

            let results = self.run_tools(ctx, &tool_calls, &cancel).await?;
            self.sessions
                .append(
                    session,
                    ChatMessage::User(results.into_iter().map(UserContent::ToolResult).collect()),
                )
                .await?;
        }

        Err(AgentError::MaxTurnsExceeded(self.max_turns.get()))
    }

    /// Construct one provider request from current session state and run it under the
    /// configured timeout / cancellation token.
    async fn send_one_turn(
        &self,
        session: SessionId,
        cancel: &CancellationToken,
    ) -> Result<ChatResponse, AgentError> {
        let messages = self.sessions.snapshot(session).await?;
        // §6: an empty history would mean the caller jumped straight into a turn loop
        // without seeding the user prompt — unrepresentable, but cheap to assert.
        assert!(
            !messages.is_empty(),
            "session must contain at least the user prompt"
        );

        let system = self.memory.system_prompt(session).await?;
        let request = ChatRequest {
            model: self.model.clone(),
            system,
            messages,
            tools: self.tools.specs(),
            max_output_tokens: self.max_output_tokens,
        };

        let send = self.provider.send(request);
        tokio::select! {
            biased;
            () = cancel.cancelled() => Err(AgentError::Cancelled),
            r = timeout(self.provider_timeout, send) => match r {
                Ok(Ok(resp)) => Ok(resp),
                Ok(Err(e)) => Err(AgentError::Provider(e)),
                Err(_) => Err(AgentError::ProviderTimeout),
            },
        }
    }

    /// Execute every tool call from the assistant turn, returning a `ToolResult` for
    /// each — never short-circuits on the first error so the model receives a complete
    /// picture of what happened.
    async fn run_tools(
        &self,
        ctx: TurnContext,
        calls: &[&ToolCall],
        cancel: &CancellationToken,
    ) -> Result<Vec<ToolResult>, AgentError> {
        let mut out = Vec::with_capacity(calls.len());
        for call in calls {
            if cancel.is_cancelled() {
                return Err(AgentError::Cancelled);
            }
            let tool_ctx = ToolContext {
                session_id: ctx.session_id,
                turn_index: ctx.turn_index,
                call,
            };
            self.guard(self.hooks.before_tool(tool_ctx).await?)?;
            let result = self.run_one_tool(call, cancel).await;
            self.guard(self.hooks.after_tool(tool_ctx, &result).await?)?;
            out.push(result);
        }
        Ok(out)
    }

    /// Resolve and run a single tool. All failure modes (unknown tool, timeout, tool
    /// error) get folded into a `ToolResult { is_error: true }` so the model can reason
    /// about them. Cancellation is the only condition that bubbles upward.
    async fn run_one_tool(&self, call: &ToolCall, cancel: &CancellationToken) -> ToolResult {
        let id = call.id.clone();
        let Some(tool) = self.tools.get(call.name.as_str()) else {
            warn!(relay.tool = %call.name, "tool.unknown");
            return error_result(id, format!("unknown tool: {}", call.name));
        };

        let exec = tool.execute(call.input.clone());
        let outcome = tokio::select! {
            biased;
            () = cancel.cancelled() => return error_result(id, "cancelled".into()),
            r = timeout(self.tool_timeout, exec) => r,
        };

        match outcome {
            Ok(Ok(output)) => {
                debug!(relay.tool = %call.name, bytes = output.len(), "tool.result.ok");
                ToolResult {
                    call_id: id,
                    output,
                    is_error: false,
                }
            }
            Ok(Err(e)) => {
                warn!(relay.tool = %call.name, error = %e, "tool.result.err");
                error_result(id, e.to_string())
            }
            Err(_) => {
                warn!(relay.tool = %call.name, "tool.timeout");
                error_result(id, format!("tool `{}` timed out", call.name))
            }
        }
    }

    fn guard(&self, decision: HookDecision) -> Result<(), AgentError> {
        match decision {
            HookDecision::Continue => Ok(()),
            HookDecision::Deny { reason } => Err(AgentError::HookDenied(reason)),
        }
    }
}

fn error_result(call_id: ToolCallId, message: String) -> ToolResult {
    ToolResult {
        call_id,
        output: message,
        is_error: true,
    }
}

// Pull AssistantContent into scope so doc-cross-refs in this module's tests resolve.
#[allow(dead_code)]
fn _doc_link() -> Option<AssistantContent> {
    None
}
