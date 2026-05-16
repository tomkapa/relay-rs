//! Per-turn execution: provider call, tool calls, request assembly.
//!
//! Lifecycle (`reply` / `resume` / `run_loop`) lives in [`super::core`]; this
//! module owns the body of one iteration of the turn loop.

use tokio::time::timeout;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::hook::{ToolContext, TurnContext};
use crate::provider::{
    ChatMessage, ChatRequest, ChatResponse, ToolCall, ToolCallId, ToolResult, UserContent,
};
use crate::runtime::{PromptRequestId, RequestKind, RequestKindPayload};
use crate::session::SessionId;
use crate::tools::{
    SharedTool, TOOL_RESULT_MAX_BYTES, ToolBox, ToolCallContext, truncate_to_char_boundary,
};
use crate::types::{MessageSender, Participant, TurnIndex};

use super::core::{Agent, send_message_tool_name};
use super::error::AgentError;
use super::limits::MAX_TOOL_CALLS_PER_TURN;
use super::log;
use super::observer::SharedTurnObserver;
use super::outcome::viewer_kind;

impl Agent {
    /// Run one provider call + its tool-call follow-up. Returns `Some(text)` when
    /// the turn ends with a final answer; `None` to continue the loop.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn run_turn(
        &self,
        ctx: TurnContext,
        viewer: Participant,
        counterpart: Participant,
        viewer_as_sender: MessageSender,
        root_request_id: PromptRequestId,
        request_id: PromptRequestId,
        kind_payload: &RequestKindPayload,
        send_message_calls: &mut usize,
        cancel: &CancellationToken,
        observer: Option<&SharedTurnObserver>,
    ) -> Result<Option<String>, AgentError> {
        self.hooks().before_turn(ctx).await?.into_result()?;
        let response = self
            .send_one_turn(ctx.session_id, viewer, kind_payload, cancel)
            .await?;
        self.hooks()
            .after_turn(ctx, &response)
            .await?
            .into_result()?;

        for block in &response.content {
            log::assistant_block(ctx.turn_index.get(), block);
        }
        if let Some(obs) = observer {
            for block in &response.content {
                obs.on_assistant(block).await;
            }
        }

        self.sessions()
            .append(
                ctx.session_id,
                viewer_as_sender,
                counterpart,
                ChatMessage::Assistant(response.content.clone()),
                request_id,
            )
            .await?;

        let tool_calls = response.tool_calls();
        tracing::Span::current().record("relay.tool_calls.count", tool_calls.len());
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

        let tool_ctx = ToolCallContext {
            session_id: ctx.session_id,
            viewer,
            root_request_id,
            request_id,
            kind_payload: kind_payload.clone(),
        };
        // Counted regardless of tool error — the model already saw the failure
        // via the tool result; the worker's ping-pong guard cares only about
        // attempts to deliver.
        for call in &tool_calls {
            if call.name.as_str() == send_message_tool_name() {
                *send_message_calls += 1;
            }
        }
        let results = self
            .run_tools(
                ctx,
                &tool_calls,
                self.tools(),
                kind_payload.kind(),
                &tool_ctx,
                cancel,
                observer,
            )
            .await?;
        // Sender = `System` so the row renders to viewer-as-User without
        // claiming the human authored the result.
        self.sessions()
            .append(
                ctx.session_id,
                MessageSender::System,
                viewer,
                ChatMessage::User(results.into_iter().map(UserContent::ToolResult).collect()),
                request_id,
            )
            .await?;
        Ok(None)
    }

    pub(super) async fn send_one_turn(
        &self,
        session: SessionId,
        viewer: Participant,
        kind_payload: &RequestKindPayload,
        cancel: &CancellationToken,
    ) -> Result<ChatResponse, AgentError> {
        let request = self
            .build_chat_request(session, viewer, kind_payload)
            .await?;
        self.call_provider(request, self.provider_timeout(), cancel)
            .await
    }

    /// Single LLM provider entry point. Every code path that talks to a
    /// model — normal turn, reflection, resolution — funnels through here
    /// so timeout, cancellation, and error mapping live in one place.
    pub(super) async fn call_provider(
        &self,
        request: ChatRequest,
        timeout_after: std::time::Duration,
        cancel: &CancellationToken,
    ) -> Result<ChatResponse, AgentError> {
        let send = self.provider().send(request);
        tokio::select! {
            biased;
            () = cancel.cancelled() => Err(AgentError::Cancelled),
            r = timeout(timeout_after, send) => match r {
                Ok(Ok(resp)) => Ok(resp),
                Ok(Err(e)) => Err(AgentError::Provider(e)),
                Err(_) => Err(AgentError::ProviderTimeout),
            },
        }
    }

    /// Assemble the per-turn provider request: own-session history, optional
    /// parent-session prefix, system prompt, tool specs.
    #[tracing::instrument(
        skip_all,
        name = "session.context.build",
        fields(
            relay.session.id = %session,
            relay.viewer = %viewer,
            relay.viewer.kind = viewer_kind(viewer),
            relay.history.count = tracing::field::Empty,
            relay.parent_session.included = tracing::field::Empty,
            relay.parent_session.history.count = tracing::field::Empty,
            relay.system_prompt.bytes = tracing::field::Empty,
            relay.messages.count = tracing::field::Empty,
        ),
    )]
    async fn build_chat_request(
        &self,
        session: SessionId,
        viewer: Participant,
        kind_payload: &RequestKindPayload,
    ) -> Result<ChatRequest, AgentError> {
        let kind = kind_payload.kind();
        let span = tracing::Span::current();
        let own = self.sessions().snapshot(session, viewer).await?;
        assert!(
            !own.is_empty(),
            "session must contain at least the user prompt"
        );
        span.record("relay.history.count", own.len());

        // Prepend the immediate parent session's history when the viewer
        // participates in the parent — i.e. the agent's own conversation
        // continues across the fork (e.g. `default` reading `human↔default`
        // while processing a reply from `default↔translator`). Foreign viewers
        // get an empty parent history; framing comes through `send_message`'s
        // `context_summary`, with `get_session` for deeper lookups.
        let parent = self
            .sessions()
            .parent_history_for_viewer(session, viewer)
            .await?;
        span.record("relay.parent_session.included", !parent.is_empty());
        span.record("relay.parent_session.history.count", parent.len());

        let mut messages: Vec<ChatMessage> = Vec::with_capacity(parent.len() + own.len());
        messages.extend(parent);
        messages.extend(own);

        let system = self
            .memory()
            .system_prompt(session, viewer, kind_payload)
            .await?;
        span.record("relay.system_prompt.bytes", system.len());
        span.record("relay.messages.count", messages.len());

        let tools = self.tools().specs_for(kind);
        Ok(ChatRequest {
            model: self.model().clone(),
            system,
            messages,
            tools,
            max_output_tokens: self.max_output_tokens(),
        })
    }

    /// Execute every tool call from the assistant turn against `tools`,
    /// returning a `ToolResult` for each — never short-circuits, so the
    /// model receives a complete picture of what happened. The toolbox is
    /// mode-filtered, so different turn modes (normal, reflection,
    /// resolution) can present different closed sets to the model.
    ///
    /// Consecutive concurrency-safe calls fan out via
    /// [`futures::future::join_all`]; an unsafe (or unknown) call forms
    /// a barrier. `join_all` preserves input order.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn run_tools(
        &self,
        ctx: TurnContext,
        calls: &[&ToolCall],
        tools: &ToolBox,
        kind: RequestKind,
        tool_ctx: &ToolCallContext,
        cancel: &CancellationToken,
        observer: Option<&SharedTurnObserver>,
    ) -> Result<Vec<ToolResult>, AgentError> {
        let resolved: Vec<Option<SharedTool>> = calls
            .iter()
            .map(|c| tools.get_for(kind, c.name.as_str()))
            .collect();
        let classes: Vec<bool> = resolved
            .iter()
            .map(|t| t.as_ref().is_some_and(|x| x.concurrency_safe()))
            .collect();
        let batches = plan_batches(&classes);

        let mut out: Vec<ToolResult> = Vec::with_capacity(calls.len());
        for batch in batches {
            if cancel.is_cancelled() {
                return Err(AgentError::Cancelled);
            }
            let results = futures::future::join_all(batch.map(|i| {
                self.run_call_with_hooks(
                    ctx,
                    calls[i],
                    resolved[i].clone(),
                    kind,
                    tool_ctx,
                    cancel,
                    observer,
                )
            }))
            .await;
            for r in results {
                out.push(r?);
            }
        }
        Ok(out)
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_call_with_hooks(
        &self,
        ctx: TurnContext,
        call: &ToolCall,
        tool: Option<SharedTool>,
        kind: RequestKind,
        tool_ctx: &ToolCallContext,
        cancel: &CancellationToken,
        observer: Option<&SharedTurnObserver>,
    ) -> Result<ToolResult, AgentError> {
        let hook_ctx = ToolContext {
            session_id: ctx.session_id,
            turn_index: ctx.turn_index,
            call,
        };
        self.hooks().before_tool(hook_ctx).await?.into_result()?;
        let result = self.run_one_tool(call, tool, kind, tool_ctx, cancel).await;
        self.hooks()
            .after_tool(hook_ctx, &result)
            .await?
            .into_result()?;
        log::tool_result(ctx.turn_index.get(), &result);
        if let Some(obs) = observer {
            obs.on_tool_result(&result).await;
        }
        Ok(result)
    }

    /// Resolve and run a single tool. All failure modes (unknown tool, timeout,
    /// tool error) fold into a `ToolResult { is_error: true }` so the model
    /// can reason about them. Cancellation is the only condition that bubbles.
    #[tracing::instrument(
        skip_all,
        name = "execute_tool",
        fields(
            gen_ai.operation.name = "execute_tool",
            gen_ai.tool.name = %call.name,
            gen_ai.tool.call.id = %call.id.as_str(),
            relay.tool = %call.name,
            relay.session.id = %tool_ctx.session_id.as_uuid(),
        ),
    )]
    async fn run_one_tool(
        &self,
        call: &ToolCall,
        tool: Option<SharedTool>,
        kind: RequestKind,
        tool_ctx: &ToolCallContext,
        cancel: &CancellationToken,
    ) -> ToolResult {
        let id = call.id.clone();
        let Some(tool) = tool else {
            warn!(relay.tool = %call.name, "tool.unknown");
            return error_result(
                id,
                format!(
                    "unknown tool for kind={kind}: {name}",
                    kind = kind.as_str(),
                    name = call.name
                ),
            );
        };

        let exec = tool.execute(call.input.clone(), tool_ctx);
        let outcome = tokio::select! {
            biased;
            () = cancel.cancelled() => return error_result(id, "cancelled".into()),
            r = timeout(self.tool_timeout(), exec) => r,
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
}

fn error_result(call_id: ToolCallId, message: String) -> ToolResult {
    // Defence in depth: cap error messages too. A misbehaving tool could
    // otherwise embed an upstream body and blow the budget.
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

/// Helper to construct a `TurnIndex` from a loop counter inside the
/// bounded `0..max_turns` range.
pub(super) fn turn_index(turn: u32) -> TurnIndex {
    TurnIndex::try_from(turn).expect("invariant: max_turns is bounded so loop index fits TurnIndex")
}

/// Fuse consecutive `true` entries into a single range; each `false`
/// becomes a singleton. Preserves input order; covers `0..classes.len()`
/// exactly once.
pub(super) fn plan_batches(classes: &[bool]) -> Vec<std::ops::Range<usize>> {
    let mut out: Vec<std::ops::Range<usize>> = Vec::new();
    let mut i = 0;
    while i < classes.len() {
        let mut j = i + 1;
        if classes[i] {
            while j < classes.len() && classes[j] {
                j += 1;
            }
        }
        out.push(i..j);
        i = j;
    }
    out
}

#[cfg(test)]
mod plan_batches_tests {
    use super::plan_batches;

    #[test]
    fn empty_input_yields_no_batches() {
        let batches = plan_batches(&[]);
        assert!(batches.is_empty());
    }

    #[test]
    fn single_safe_call_is_one_singleton_batch() {
        let batches = plan_batches(&[true]);
        assert_eq!(batches, vec![0..1]);
    }

    #[test]
    fn single_unsafe_call_is_one_singleton_batch() {
        let batches = plan_batches(&[false]);
        assert_eq!(batches, vec![0..1]);
    }

    #[test]
    fn consecutive_safe_calls_fuse_into_one_batch() {
        let batches = plan_batches(&[true, true, true]);
        assert_eq!(batches, vec![0..3]);
    }

    #[test]
    fn unsafe_call_breaks_the_batch() {
        // [A_safe, B_safe, C_unsafe, D_safe, E_safe, F_safe]
        // → [{A,B}, {C}, {D,E,F}]
        let batches = plan_batches(&[true, true, false, true, true, true]);
        assert_eq!(batches, vec![0..2, 2..3, 3..6]);
    }

    #[test]
    fn alternating_unsafe_and_safe_yields_singletons_then_runs() {
        let batches = plan_batches(&[false, true, false, true, true]);
        assert_eq!(batches, vec![0..1, 1..2, 2..3, 3..5]);
    }

    #[test]
    fn every_call_is_visited_exactly_once_in_order() {
        let classes = [true, false, true, true, false, false, true];
        let batches = plan_batches(&classes);
        let mut covered: Vec<usize> = Vec::new();
        for b in batches {
            covered.extend(b);
        }
        assert_eq!(covered, (0..classes.len()).collect::<Vec<_>>());
    }
}
