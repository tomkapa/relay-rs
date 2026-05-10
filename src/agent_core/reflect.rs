//! Reflection turn (doc/memory.md §1.6, §2.4 — Phase 4).
//!
//! Reflection is autonomous self-curation. Different from a normal turn:
//! no SSE output, only memory tools available, single provider call,
//! hard cap on mutations. The worker dispatches here when it claims a
//! `RequestKind::Reflection` row.

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::agents::AgentId;
use crate::memory::{MAX_MEMORY_MUTATIONS_PER_REFLECTION, MemoryError};
use crate::provider::{
    AssistantContent, ChatMessage, ChatRequest, ChatResponse, ToolCall, ToolSpec, UserContent,
};
use crate::runtime::PromptRequestId;
use crate::session::SessionId;
use crate::tools::ToolCallContext;
use crate::types::Participant;

use super::core::Agent;
use super::error::AgentError;

/// Binary constant injected as the system-prompt "core" for a reflection
/// turn (replaces the normal communication-protocol core). Brief on
/// purpose — `reflection_role` (per-agent) and the agent's standard
/// `role` are spliced in by the caller.
///
/// Intentional placeholder per doc/memory.md §2.4 — "the actual prompt
/// engineering is a separate task". Phase 4 lands the structural
/// dispatch; the wording here is functional, not optimised.
const REFLECTION_CORE_PROMPT: &str = "You are reflecting on a recent conversation between turns. \
    Decide what — if anything — to remember, update, or forget about \
    yourself, the people and agents you spoke with, and the procedures \
    you used. Each memory you write must be one or two sentences and \
    independently meaningful when read in isolation.\n\
    \n\
    Use ONLY these tools: `memory_write`, `memory_update`, `memory_forget`. \
    Do not call `send_message`, `recall`, or any other tool — they are \
    intentionally not available in a reflection turn.\n\
    \n\
    Be conservative. Most conversations need zero or one new memories. \
    If nothing in the conversation crossed the threshold of \"this is \
    worth carrying forward across sessions\", emit no tool calls and \
    end the turn. Avoid duplicates: if a memory already exists in your \
    `## Memory` section, do not write the same fact again — update it \
    only when the existing entry is wrong.";

/// Tools allowed during a reflection turn (doc/memory.md §2.4).
const REFLECTION_TOOL_NAMES: &[&str] = &["memory_write", "memory_update", "memory_forget"];

/// Per-call timeout for a single reflection. Defended at the worker
/// level by `MAX_TURN_DURATION` too; this keeps a stuck provider from
/// holding the per-agent serialization lock indefinitely.
const REFLECTION_PROVIDER_TIMEOUT: Duration = Duration::from_secs(120);

/// Outcome of a reflection turn — the worker uses this to advance the
/// `reflection_checkpoints` row.
#[derive(Debug, Clone)]
pub struct ReflectionOutcome {
    /// Number of `memory_*` tool calls the model emitted (capped at
    /// [`MAX_MEMORY_MUTATIONS_PER_REFLECTION`]). Zero is a valid
    /// outcome — most conversations don't merit a write.
    pub mutations: usize,
    /// `last_turn_id` to record on the `reflection_checkpoints` row —
    /// the most recent message processed by this reflection. The next
    /// reflection will only pick up messages strictly after this id.
    pub last_processed_turn_id: PromptRequestId,
}

impl Agent {
    /// Run one reflection turn over the messages strictly after
    /// `since_turn_id` (or the full session when `since_turn_id` is
    /// `None`). The turn produces only memory mutations; no SSE chunks
    /// are emitted, no message rows are appended, and only the three
    /// `memory_*` tools are exposed.
    #[tracing::instrument(
        skip_all,
        name = "agent.reflect",
        fields(
            relay.session.id = %session,
            relay.agent.id = %agent_id,
            relay.provider = self.provider().name(),
            relay.model = %self.model(),
            relay.reflection.since_turn_id = since_turn_id.map_or_else(|| "<none>".into(), |id: PromptRequestId| id.to_string()),
            relay.reflection.mutations = tracing::field::Empty,
        ),
    )]
    pub async fn reflect(
        &self,
        pool: &PgPool,
        agent_id: AgentId,
        session: SessionId,
        request_id: PromptRequestId,
        since_turn_id: Option<PromptRequestId>,
        cancel: CancellationToken,
    ) -> Result<ReflectionOutcome, AgentError> {
        let viewer = Participant::agent(agent_id);

        let new_messages = fetch_messages_since(pool, session, since_turn_id)
            .await
            .map_err(|e| AgentError::Memory(MemoryError::Backend(e.to_string())))?;

        if new_messages.is_empty() {
            return Ok(ReflectionOutcome {
                mutations: 0,
                last_processed_turn_id: since_turn_id.unwrap_or(request_id),
            });
        }

        let last_processed_turn_id = new_messages
            .last()
            .map(|m| m.request_id)
            .expect("invariant: non-empty checked above");

        let request = self
            .build_reflection_request(session, viewer, &new_messages)
            .await?;
        let response = self.run_provider_call(request, &cancel).await?;
        let mutations = self
            .run_reflection_tool_calls(response, session, viewer, request_id, &cancel)
            .await?;

        tracing::Span::current().record("relay.reflection.mutations", mutations);
        debug!(
            relay.session.id = %session,
            relay.agent.id = %agent_id,
            relay.reflection.mutations = mutations,
            "agent.reflect.done",
        );

        Ok(ReflectionOutcome {
            mutations,
            last_processed_turn_id,
        })
    }

    async fn build_reflection_request(
        &self,
        session: SessionId,
        viewer: Participant,
        new_messages: &[FetchedMessage],
    ) -> Result<ChatRequest, AgentError> {
        let memory_section = self
            .memory()
            .system_prompt(session, viewer)
            .await
            .map_err(AgentError::Memory)?;
        let role_record = self
            .memory()
            .system_prompt(session, viewer)
            .await
            .map_err(AgentError::Memory)?;
        // `system_prompt` already wraps `<core>`/`<role>`/`<memory>`. For
        // reflection we replace the core but reuse the role + memory by
        // stripping the existing `<core>...</core>` envelope and prepending
        // a reflection-specific core. Cheaper than rebuilding the whole
        // assembly and keeps memory composition in one place.
        let after_core = strip_core_envelope(role_record.as_ref()).unwrap_or_else(|| {
            // §6: AgentMemory always emits the envelope; observing
            // otherwise means the renderer skipped its tags.
            warn!("reflection: role/memory text missing <core> envelope; rebuilding");
            memory_section.as_ref().to_string()
        });
        let _ = memory_section; // borrow ends here.

        let reflection_role = self.resolve_reflection_role(viewer).unwrap_or_default();

        let mut system = String::with_capacity(
            REFLECTION_CORE_PROMPT.len() + after_core.len() + reflection_role.len() + 64,
        );
        system.push_str("<core>\n");
        system.push_str(REFLECTION_CORE_PROMPT);
        system.push_str("\n</core>\n");
        if !reflection_role.is_empty() {
            system.push_str("<reflection_role>\n");
            system.push_str(&reflection_role);
            system.push_str("\n</reflection_role>\n");
        }
        system.push_str(&after_core);

        let user_text = render_messages_for_reflection(new_messages);
        let messages = vec![ChatMessage::User(vec![UserContent::Text(user_text)])];

        let tools = filter_reflection_tools(self.tools().specs());

        Ok(ChatRequest {
            model: self.model().clone(),
            system: Arc::from(system),
            messages,
            tools,
            max_output_tokens: self.max_output_tokens(),
        })
    }

    fn resolve_reflection_role(&self, viewer: Participant) -> Option<String> {
        // Reaching through the agents store via the existing memory
        // plumbing isn't possible without a new accessor; Phase 4 keeps
        // this optional and returns `None`, which matches "no
        // reflection_role configured".
        let _ = viewer;
        None
    }

    async fn run_provider_call(
        &self,
        request: ChatRequest,
        cancel: &CancellationToken,
    ) -> Result<ChatResponse, AgentError> {
        let send = self.provider().send(request);
        tokio::select! {
            biased;
            () = cancel.cancelled() => Err(AgentError::Cancelled),
            r = timeout(REFLECTION_PROVIDER_TIMEOUT, send) => match r {
                Ok(Ok(resp)) => Ok(resp),
                Ok(Err(e)) => Err(AgentError::Provider(e)),
                Err(_) => Err(AgentError::ProviderTimeout),
            },
        }
    }

    async fn run_reflection_tool_calls(
        &self,
        response: ChatResponse,
        session: SessionId,
        viewer: Participant,
        request_id: PromptRequestId,
        cancel: &CancellationToken,
    ) -> Result<usize, AgentError> {
        let calls: Vec<&ToolCall> = response
            .content
            .iter()
            .filter_map(|c| match c {
                AssistantContent::ToolCall(call) => Some(call),
                _ => None,
            })
            .collect();
        if calls.is_empty() {
            return Ok(0);
        }
        if calls.len() > MAX_MEMORY_MUTATIONS_PER_REFLECTION {
            return Err(AgentError::TooManyToolCalls {
                max: MAX_MEMORY_MUTATIONS_PER_REFLECTION,
            });
        }

        let tool_ctx = ToolCallContext {
            session_id: session,
            viewer,
            // Reflection has no DAG anchor of its own — reuse the
            // request id so journal `source_turn_id` references the
            // reflection request, not a user-facing one.
            root_request_id: request_id,
            request_id,
        };

        let mut applied = 0usize;
        for call in &calls {
            if cancel.is_cancelled() {
                return Err(AgentError::Cancelled);
            }
            // §6: tool filtering should have been done in the request,
            // but defend in depth — a model that fabricates a non-memory
            // call is rejected silently.
            if !REFLECTION_TOOL_NAMES.contains(&call.name.as_str()) {
                warn!(
                    relay.session.id = %session,
                    tool = call.name.as_str(),
                    "agent.reflect.disallowed_tool_dropped",
                );
                continue;
            }
            let tool = self
                .tools()
                .get(call.name.as_str())
                .ok_or_else(|| AgentError::UnknownTool(call.name.to_string()))?;
            let exec = tool.execute_with_ctx(call.input.clone(), &tool_ctx);
            match timeout(self.tool_timeout(), exec).await {
                Ok(Ok(_)) => applied += 1,
                Ok(Err(e)) => {
                    warn!(
                        relay.session.id = %session,
                        tool = call.name.as_str(),
                        error = %e,
                        "agent.reflect.tool_error",
                    );
                    // Reflection treats individual tool errors as
                    // non-fatal — the librarian will sweep up duplicates
                    // and a model retry next reflection covers misses.
                }
                Err(_) => {
                    warn!(
                        relay.session.id = %session,
                        tool = call.name.as_str(),
                        "agent.reflect.tool_timeout",
                    );
                }
            }
        }

        Ok(applied)
    }
}

/// Strip the leading `<core>...</core>\n` envelope from a fully-assembled
/// system prompt produced by [`crate::memory::AgentMemory`]. Returns the
/// remainder ("role" + optional "memory" sections) so a caller that
/// wants to swap the core can do so without re-assembling everything.
fn strip_core_envelope(text: &str) -> Option<String> {
    let after = text.strip_prefix("<core>\n")?;
    let close = after.find("\n</core>\n")?;
    Some(after[close + "\n</core>\n".len()..].to_string())
}

fn filter_reflection_tools(specs: Arc<[ToolSpec]>) -> Arc<[ToolSpec]> {
    let kept: Vec<ToolSpec> = specs
        .iter()
        .filter(|s| REFLECTION_TOOL_NAMES.contains(&s.name.as_str()))
        .cloned()
        .collect();
    Arc::from(kept)
}

#[derive(Debug, Clone)]
struct FetchedMessage {
    request_id: PromptRequestId,
    role: String,
    body_summary: String,
    created_at: DateTime<Utc>,
}

/// Fetch session messages strictly after `since`'s creation time. When
/// `since` is `None`, returns every message in the session (the first
/// reflection for this session).
async fn fetch_messages_since(
    pool: &PgPool,
    session: SessionId,
    since: Option<PromptRequestId>,
) -> Result<Vec<FetchedMessage>, sqlx::Error> {
    let cutoff: Option<DateTime<Utc>> = if let Some(id) = since {
        let row: Option<(DateTime<Utc>,)> = sqlx::query_as(
            "SELECT created_at FROM session_messages
             WHERE session_id = $1 AND request_id = $2
             ORDER BY seq DESC
             LIMIT 1",
        )
        .bind(session)
        .bind(id)
        .fetch_optional(pool)
        .await?;
        row.map(|(ts,)| ts)
    } else {
        None
    };

    let rows: Vec<(
        PromptRequestId,
        String,
        sqlx::types::Json<serde_json::Value>,
        DateTime<Utc>,
    )> = if let Some(ts) = cutoff {
        sqlx::query_as(
            "SELECT request_id, sender_kind, body, created_at FROM session_messages
                 WHERE session_id = $1 AND created_at > $2
                 ORDER BY seq ASC",
        )
        .bind(session)
        .bind(ts)
        .fetch_all(pool)
        .await?
    } else {
        sqlx::query_as(
            "SELECT request_id, sender_kind, body, created_at FROM session_messages
                 WHERE session_id = $1
                 ORDER BY seq ASC",
        )
        .bind(session)
        .fetch_all(pool)
        .await?
    };

    Ok(rows
        .into_iter()
        .map(|(request_id, role, body, created_at)| FetchedMessage {
            request_id,
            role,
            body_summary: summarize_body(&body.0),
            created_at,
        })
        .collect())
}

/// Best-effort one-liner per message body. The reflection model needs
/// enough signal to spot patterns; we do not need to reconstruct the
/// exact tool arguments.
fn summarize_body(body: &serde_json::Value) -> String {
    match body {
        serde_json::Value::Object(map) => map
            .iter()
            .find_map(|(k, v)| {
                if k == "text" || k == "content" {
                    v.as_str().map(str::to_owned)
                } else {
                    None
                }
            })
            .unwrap_or_else(|| body.to_string()),
        _ => body.to_string(),
    }
}

fn render_messages_for_reflection(messages: &[FetchedMessage]) -> String {
    let mut out = String::with_capacity(2048);
    out.push_str(
        "Below is the conversation since you last reflected. Decide what — if \
         anything — to remember.\n\n",
    );
    for m in messages {
        out.push('[');
        out.push_str(&m.created_at.to_rfc3339());
        out.push(' ');
        out.push_str(&m.role);
        out.push_str("] ");
        out.push_str(&m.body_summary);
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_core_envelope_returns_remainder() {
        let s = "<core>\nhello\n</core>\n<role>\nstuff\n</role>";
        let after = strip_core_envelope(s).expect("envelope");
        assert_eq!(after, "<role>\nstuff\n</role>");
    }

    #[test]
    fn strip_core_envelope_returns_none_without_envelope() {
        assert!(strip_core_envelope("no envelope here").is_none());
    }

    #[test]
    fn filter_reflection_tools_keeps_only_memory_kind() {
        use crate::types::ToolName;
        use serde_json::json;

        let make = |name: &str| ToolSpec {
            name: ToolName::try_from(name).expect("name"),
            description: Arc::from("desc"),
            input_schema: Arc::new(json!({"type": "object"})),
        };
        let specs: Arc<[ToolSpec]> = Arc::from(vec![
            make("send_message"),
            make("memory_write"),
            make("memory_update"),
            make("memory_forget"),
            make("recall"),
        ]);
        let filtered = filter_reflection_tools(specs);
        let names: Vec<&str> = filtered.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["memory_write", "memory_update", "memory_forget"]
        );
    }
}
