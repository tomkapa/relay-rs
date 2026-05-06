//! Per-session [`Memory`] backed by the agents registry.
//!
//! On each turn:
//! 1. Resolve the session's `agent_id` via [`SessionStore::agent_id`].
//! 2. Load the agent's role-specific prompt through [`AgentPromptCache`].
//! 3. Concatenate it with the in-code core prompt as
//!    `<core>...</core>\n<role>{prompt}</role>` so the model sees a clear
//!    separation between universal personality and role-specific job
//!    description.
//!
//! Agents can be edited at runtime; the cache TTL bounds how long stale text
//! lingers ([`crate::agents::AGENT_PROMPT_CACHE_TTL`]).

use std::sync::Arc;

use async_trait::async_trait;

use crate::agents::{AgentPromptCache, SharedAgentStore};
use crate::session::{SessionId, SharedSessionStore};

use super::traits::{Memory, MemoryError};

/// Stable XML-ish tags wrapping each prompt section. Marked `pub` so consumers
/// (e.g. tests, docs) can assert on the wire format if they need to.
pub const CORE_TAG_OPEN: &str = "<core>\n";
pub const CORE_TAG_CLOSE: &str = "\n</core>\n";
pub const ROLE_TAG_OPEN: &str = "<role>\n";
pub const ROLE_TAG_CLOSE: &str = "\n</role>";

/// Composite memory that assembles the system prompt from a constant core and a
/// per-agent role string fetched on demand.
pub struct AgentMemory {
    sessions: SharedSessionStore,
    agents: SharedAgentStore,
    cache: Arc<AgentPromptCache>,
    core: Arc<str>,
}

impl AgentMemory {
    #[must_use]
    pub fn new(
        sessions: SharedSessionStore,
        agents: SharedAgentStore,
        cache: Arc<AgentPromptCache>,
        core: impl Into<Arc<str>>,
    ) -> Self {
        Self {
            sessions,
            agents,
            cache,
            core: core.into(),
        }
    }
}

impl std::fmt::Debug for AgentMemory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentMemory")
            .field("core_len", &self.core.len())
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl Memory for AgentMemory {
    async fn system_prompt(&self, session: SessionId) -> Result<Arc<str>, MemoryError> {
        let agent_id = self.sessions.agent_id(session).await?;
        let role = self.cache.get_or_load(agent_id, &self.agents).await?;

        let core = self.core.as_ref();
        let role_str = role.as_str();
        let mut out = String::with_capacity(
            CORE_TAG_OPEN.len()
                + core.len()
                + CORE_TAG_CLOSE.len()
                + ROLE_TAG_OPEN.len()
                + role_str.len()
                + ROLE_TAG_CLOSE.len(),
        );
        out.push_str(CORE_TAG_OPEN);
        out.push_str(core);
        out.push_str(CORE_TAG_CLOSE);
        out.push_str(ROLE_TAG_OPEN);
        out.push_str(role_str);
        out.push_str(ROLE_TAG_CLOSE);

        Ok(Arc::from(out))
    }
}
