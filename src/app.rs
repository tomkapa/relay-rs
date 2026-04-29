//! Composition root.
//!
//! Wires every trait object the agent needs: provider, sessions, memory, hooks, tools.
//! Each piece is constructed at startup and consumed once. Adding a new tool, swapping
//! the session backend for Postgres, or chaining a policy hook is a one-line change here
//! — the agent itself does not move.

use std::sync::Arc;
use std::time::Duration;

use reqwest::Client;

use crate::agent::{Agent, AgentBuilder};
use crate::config::Settings;
use crate::error::AppError;
use crate::hook::HookChain;
use crate::memory::{SharedMemory, StaticMemory};
use crate::provider::SharedProvider;
use crate::provider::anthropic::AnthropicProvider;
use crate::session::{InMemorySessionStore, SharedSessionStore};
use crate::tools::{ToolRegistry, WebFetchTool, WebSearchTool};

const HTTP_USER_AGENT: &str = concat!("relay-rs/", env!("CARGO_PKG_VERSION"));
const HTTP_DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
const HTTP_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

const DEFAULT_SYSTEM_PROMPT: &str = "You are Relay, a helpful AI agent. \
    You are concise, accurate, and prefer to verify facts using your tools \
    before answering when the answer is not obvious. \
    When you call a tool, briefly state why before the call. \
    When you have enough information, give the user a clear final answer.";

/// Build a fully-wired [`Agent`] from configuration. The returned agent owns all of its
/// shared collaborators (provider, sessions, memory, hooks, tools) — it can be cloned
/// freely and shared across tasks.
pub fn build_agent(settings: Settings) -> Result<Agent, AppError> {
    let http = build_http_client()?;

    let provider = AnthropicProvider::new(
        &settings.anthropic_api_key,
        settings.anthropic_base_url.clone(),
    )?;
    let provider = SharedProvider::new(Arc::new(provider));

    let sessions = SharedSessionStore::new(Arc::new(InMemorySessionStore::new()));

    let memory: SharedMemory = Arc::new(StaticMemory::new(DEFAULT_SYSTEM_PROMPT));

    let tools = ToolRegistry::builder()
        .with(Arc::new(WebFetchTool::new()?))
        .with(Arc::new(WebSearchTool::new(
            http.clone(),
            settings.brave_search_api_key.clone(),
        )))
        .build();

    let agent = AgentBuilder::new(provider, sessions, memory, settings.model.clone())?
        .with_tools(tools)
        .with_hooks(HookChain::new())
        .build();

    Ok(agent)
}

fn build_http_client() -> Result<Client, reqwest::Error> {
    Client::builder()
        .timeout(HTTP_DEFAULT_TIMEOUT)
        .connect_timeout(HTTP_CONNECT_TIMEOUT)
        .user_agent(HTTP_USER_AGENT)
        .build()
}
