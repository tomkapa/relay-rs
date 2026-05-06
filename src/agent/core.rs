use std::time::Duration;

use tokio::time::timeout;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, instrument, warn};

use crate::agents::AgentId;
use crate::clock::SharedClock;
use crate::hook::{HookChain, HookDecision, ToolContext, TurnContext};
use crate::memory::SharedMemory;
use crate::provider::{
    ChatMessage, ChatRequest, ChatResponse, SharedProvider, ToolCall, ToolCallId, ToolResult,
    UserContent,
};
use crate::session::{SessionId, SharedSessionStore};
use crate::tools::{TOOL_RESULT_MAX_BYTES, ToolBox, truncate_to_char_boundary};
use crate::types::{MaxOutputTokens, MaxTurns, ModelId, Prompt, TurnIndex};

use super::error::AgentError;
use super::limits::MAX_TOOL_CALLS_PER_TURN;
use super::observer::SharedTurnObserver;

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
    tools: ToolBox,
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
        tools: ToolBox,
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

    /// Allocate a fresh session bound to `agent_id`. Returns the handle the caller
    /// passes to [`reply`](Self::reply) for subsequent turns.
    pub async fn start_session(&self, agent_id: AgentId) -> Result<SessionId, AgentError> {
        Ok(self.sessions.create(agent_id).await?)
    }

    /// Drive a batch of user prompts (a single user message with one text block per
    /// prompt) to a final assistant text answer, running tool calls in between turns.
    /// Honours `cancel`: returns [`AgentError::Cancelled`] at the next checkpoint.
    ///
    /// `observer` (if `Some`) is notified at every assistant content block and every
    /// tool result so the SSE pipeline can stream chunks as the loop progresses
    /// rather than at the very end.
    #[instrument(
        name = "agent.reply",
        skip_all,
        fields(
            relay.session.id = %session,
            relay.provider = self.provider.name(),
            relay.model = %self.model,
            relay.batch_size = prompts.len(),
            relay.max_turns = self.max_turns.get(),
        ),
    )]
    pub async fn reply(
        &self,
        session: SessionId,
        prompts: Vec<Prompt>,
        cancel: CancellationToken,
        observer: Option<SharedTurnObserver>,
    ) -> Result<String, AgentError> {
        // §6: a worker only calls this after draining at least one prompt.
        assert!(!prompts.is_empty(), "reply requires at least one prompt");
        let user_blocks: Vec<UserContent> = prompts
            .into_iter()
            .map(|p| UserContent::Text(p.into_string()))
            .collect();
        self.sessions
            .append(session, ChatMessage::User(user_blocks))
            .await?;

        let observer = observer.as_ref();
        for turn in 0..self.max_turns.get() {
            if cancel.is_cancelled() {
                return Err(AgentError::Cancelled);
            }
            let turn_index = TurnIndex::try_from(turn)
                .expect("invariant: max_turns is bounded so loop index is a valid TurnIndex");
            let ctx = TurnContext {
                session_id: session,
                turn_index,
            };
            if let Some(text) = self.run_turn(ctx, &cancel, observer).await? {
                info!(turn, "agent.turn.final");
                return Ok(text);
            }
        }
        Err(AgentError::MaxTurnsExceeded(self.max_turns.get()))
    }

    /// Run one provider call + its tool-call follow-up. Returns `Some(text)` when the
    /// turn ends with a final answer; `None` to continue the loop.
    async fn run_turn(
        &self,
        ctx: TurnContext,
        cancel: &CancellationToken,
        observer: Option<&SharedTurnObserver>,
    ) -> Result<Option<String>, AgentError> {
        self.guard(self.hooks.before_turn(ctx).await?)?;
        let response = self.send_one_turn(ctx.session_id, cancel).await?;
        self.guard(self.hooks.after_turn(ctx, &response).await?)?;

        // Stream every assistant content block (text, reasoning, tool call) before
        // the tools run, so a UI sees the model's thinking and intent immediately.
        if let Some(obs) = observer {
            for block in &response.content {
                obs.on_assistant(block).await;
            }
        }

        self.sessions
            .append(
                ctx.session_id,
                ChatMessage::Assistant(response.content.clone()),
            )
            .await?;

        let tool_calls = response.tool_calls();
        if tool_calls.is_empty() {
            let text = response.text();
            if text.is_empty() {
                return Err(AgentError::EmptyReply);
            }
            return Ok(Some(text));
        }
        if tool_calls.len() > MAX_TOOL_CALLS_PER_TURN {
            return Err(AgentError::TooManyToolCalls {
                max: MAX_TOOL_CALLS_PER_TURN,
            });
        }

        let results = self.run_tools(ctx, &tool_calls, cancel, observer).await?;
        self.sessions
            .append(
                ctx.session_id,
                ChatMessage::User(results.into_iter().map(UserContent::ToolResult).collect()),
            )
            .await?;
        Ok(None)
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
    /// picture of what happened. Each result is streamed through `observer` (if set)
    /// the instant it lands, ahead of the next provider call.
    async fn run_tools(
        &self,
        ctx: TurnContext,
        calls: &[&ToolCall],
        cancel: &CancellationToken,
        observer: Option<&SharedTurnObserver>,
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
            if let Some(obs) = observer {
                obs.on_tool_result(&result).await;
            }
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
                if output.len() > TOOL_RESULT_MAX_BYTES {
                    warn!(
                        relay.tool = %call.name,
                        bytes = output.len(),
                        cap = TOOL_RESULT_MAX_BYTES,
                        "tool.result.too_large",
                    );
                    return error_result(
                        id,
                        format!(
                            "tool `{}` returned {} bytes; cap is {} bytes",
                            call.name,
                            output.len(),
                            TOOL_RESULT_MAX_BYTES,
                        ),
                    );
                }
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
    // Defence in depth: the cap on tool output applies just as much to error messages.
    // Tool authors who embed an upstream body in their error must respect this; we cap
    // here as a final boundary so a bad implementation cannot blow the budget.
    let mut output = message;
    if output.len() > TOOL_RESULT_MAX_BYTES {
        truncate_to_char_boundary(&mut output, TOOL_RESULT_MAX_BYTES);
    }
    ToolResult {
        call_id,
        output,
        is_error: true,
    }
}
