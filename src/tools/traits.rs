use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use thiserror::Error;

use crate::runtime::PromptRequestId;
use crate::session::SessionId;
use crate::types::{Participant, ToolName};

use super::url::UrlError;

#[derive(Debug, Error)]
pub enum ToolError {
    /// Model gave us bad arguments — wrong shape, oversize, refers to a
    /// non-existent receiver, etc. Surfacing as `invalid_input` lets the
    /// model self-correct on the next turn.
    #[error("invalid input: {0}")]
    InvalidInput(String),

    /// A downstream subsystem (session store, queue, sink, agent store) the
    /// tool depends on failed in a way that is *not* the model's fault. Kept
    /// distinct from `InvalidInput` so dashboards can separate model-driven
    /// errors from infrastructure-driven ones, and so a future retry policy
    /// can target backend faults without retrying bad-input rejections.
    #[error("backend error: {0}")]
    Backend(String),

    #[error("disallowed url: {0}")]
    DisallowedUrl(#[from] UrlError),

    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("upstream returned status {status}: {body}")]
    Upstream { status: u16, body: String },

    #[error("tool returned a result that exceeded the size cap ({max} bytes)")]
    ResultTooLarge { max: usize },

    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

/// Per-call context passed to tools that need to know who's calling them.
///
/// Threaded by the agent loop into [`Tool::execute_with_ctx`]. Most tools
/// (`web_fetch`, `web_search`, MCP tools) ignore it and fall through to
/// [`Tool::execute`]. System tools (`send_message`, `get_session`) consume it.
#[derive(Debug, Clone, Copy)]
pub struct ToolCallContext {
    /// The session that produced this tool call.
    pub session_id: SessionId,
    /// The agent currently running — its identity is what `send_message`'s
    /// receiver is checked against and what authors any messages the tool
    /// appends.
    pub viewer: Participant,
    /// DAG anchor for the conversation tree this turn belongs to. Used by
    /// `send_message` to upsert sibling sessions and bump the budget.
    pub root_request_id: PromptRequestId,
    /// The current claim's prompt request id — i.e. the row whose SSE sink
    /// is open right now. Used by `send_message` to publish
    /// `AgentMessage` chunks where the human is actually listening, instead
    /// of `root_request_id` (which can point at a long-since-quiesced
    /// prompt's closed sink on follow-up turns in a continuing thread).
    /// Postgres `LISTEN/NOTIFY` then routes the chunk by
    /// `prompt_requests.root_request_id` to the right thread fan-in.
    pub request_id: PromptRequestId,
}

/// A side-effecting capability the model can request.
///
/// Implementations must be cheap to clone (they go behind `Arc`) and must validate every
/// input from the model — never trust `input: Value` shape, parse it through `serde` into
/// a typed struct.
#[async_trait]
pub trait Tool: Send + Sync + std::fmt::Debug {
    /// Stable, lower-case identifier. Validated at registration through [`ToolName`].
    fn name(&self) -> &ToolName;

    /// Human-readable description shown to the model. Be specific — vague descriptions
    /// produce vague tool calls.
    fn description(&self) -> &str;

    /// JSON-schema description of the tool's input. Cached on the tool struct so the
    /// agent does not re-allocate it every turn.
    fn input_schema(&self) -> Arc<Value>;

    async fn execute(&self, input: Value) -> Result<String, ToolError>;

    /// Context-aware variant. Default falls through to `execute` so existing
    /// tools (web_fetch, web_search, MCP wrappers) need no change. System
    /// tools — `send_message`, `get_session` — override to consume `ctx`.
    async fn execute_with_ctx(
        &self,
        input: Value,
        _ctx: &ToolCallContext,
    ) -> Result<String, ToolError> {
        self.execute(input).await
    }
}

/// Cheap-clone alias used by the registry.
pub type SharedTool = Arc<dyn Tool>;
