//! System tools — first-party capabilities the agent invokes through the
//! tool seam.
//!
//! Two flavours:
//!
//! * **Communication** — [`SendMessageTool`] (the only delivery mechanism for
//!   messages between participants) and [`GetSessionTool`] (cross-session read
//!   scoped to the caller's DAG). Both consume [`super::ToolCallContext`] via
//!   `execute_with_ctx`.
//! * **Built-in capabilities** — [`WebFetchTool`] and [`WebSearchTool`].
//!
//! All four are registered via [`register`] from the composition root.
//! Externally-supplied tools enter through the MCP registry instead of this
//! module.

use std::sync::Arc;

use reqwest::Client;

use crate::agents::SharedAgentStore;
use crate::runtime::{SharedDagBudget, SharedPromptQueue, SharedResponseSink};
use crate::session::SharedSessionStore;
use crate::types::SecretString;

use super::registry::ToolRegistryBuilder;

mod get_session;
mod send_message;
mod web_fetch;
mod web_search;

pub use get_session::GetSessionTool;
pub use send_message::SendMessageTool;
pub use web_fetch::WebFetchTool;
pub use web_search::WebSearchTool;

/// Shared collaborators every system tool may need. The composition root
/// builds this struct once at startup and hands it to [`register`].
#[derive(Debug, Clone)]
pub struct SystemToolDeps {
    pub http: Client,
    pub brave_search_api_key: SecretString,
    pub sessions: SharedSessionStore,
    pub queue: SharedPromptQueue,
    pub dag: SharedDagBudget,
    pub agents: SharedAgentStore,
    pub sink: SharedResponseSink,
}

/// Register every system tool onto `builder`.
///
/// The single registration site for `web_fetch`, `web_search`, `send_message`,
/// and `get_session`. Adding a system tool is one new file in this directory
/// plus one `.with(...)` line here.
///
/// # Errors
///
/// Returns an error if any tool's HTTP-client construction fails.
pub fn register(
    builder: ToolRegistryBuilder,
    deps: SystemToolDeps,
) -> Result<ToolRegistryBuilder, reqwest::Error> {
    let SystemToolDeps {
        http,
        brave_search_api_key,
        sessions,
        queue,
        dag,
        agents,
        sink,
    } = deps;

    Ok(builder
        .with(Arc::new(WebFetchTool::new()?))
        .with(Arc::new(WebSearchTool::new(http, brave_search_api_key)))
        .with(Arc::new(SendMessageTool::new(
            sessions.clone(),
            queue,
            dag,
            agents,
            sink,
        )))
        .with(Arc::new(GetSessionTool::new(sessions))))
}
