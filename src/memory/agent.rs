//! Per-turn [`Memory`] backed by the agents registry + the agent's
//! memory store (doc/memory.md §2.2 — Phase 2).
//!
//! Each call resolves the viewer's role prompt (cached, TTL-bounded by
//! [`crate::agents::AGENT_PROMPT_CACHE_TTL`]) and composes the final
//! `system` field as
//! `<core>...</core>\n<role>{prompt}</role>` followed by the rendered
//! `<memory>...</memory>` section. Both the role prompt and the memory
//! section are cached per session — the latter via
//! [`SessionMemoryCache`].
//!
//! See [`SessionMemoryCache`]'s module doc for the deliberate divergence
//! from doc/memory.md's "frozen for the session's lifetime" wording: we
//! ship a TTL cache today, not session-state storage.

use std::sync::Arc;

use async_trait::async_trait;

use crate::agents::{AgentId, AgentPromptCache, SharedAgentStore};
use crate::session::SessionId;
use crate::types::Participant;

use super::composer::{MemorySection, compose_memory_section};
use super::session_cache::SessionMemoryCache;
use super::store::SharedMemoryStore;
use super::traits::{Memory, MemoryError};
use super::types::{MemoryHandle, MemoryId};

/// Stable XML-ish tags wrapping each prompt section. Marked `pub` so
/// consumers (e.g. tests, docs) can assert on the wire format if they
/// need to.
pub const CORE_TAG_OPEN: &str = "<core>\n";
pub const CORE_TAG_CLOSE: &str = "\n</core>\n";
pub const ROLE_TAG_OPEN: &str = "<role>\n";
pub const ROLE_TAG_CLOSE: &str = "\n</role>";

/// Composite memory that assembles the system prompt from a constant
/// core, a per-agent role string fetched on demand, and a per-session
/// composed memory section.
pub struct AgentMemory {
    agents: SharedAgentStore,
    prompt_cache: Arc<AgentPromptCache>,
    memory_store: SharedMemoryStore,
    session_cache: Arc<SessionMemoryCache>,
    core: Arc<str>,
}

impl AgentMemory {
    #[must_use]
    pub fn new(
        agents: SharedAgentStore,
        prompt_cache: Arc<AgentPromptCache>,
        memory_store: SharedMemoryStore,
        session_cache: Arc<SessionMemoryCache>,
        core: impl Into<Arc<str>>,
    ) -> Self {
        Self {
            agents,
            prompt_cache,
            memory_store,
            session_cache,
            core: core.into(),
        }
    }

    /// Resolve a `M-NN` handle the model produced inside `(session,
    /// agent)` back to the underlying [`MemoryId`]. Returns `None` if
    /// the handle was never minted for this session — typically a
    /// hallucinated reference or a session whose composition has been
    /// evicted from the cache.
    ///
    /// Composes the section on the spot if the cache misses; this is
    /// the same path `system_prompt` takes, so resolving against a
    /// session that just rolled past TTL is a single cache reload, not
    /// an error.
    pub async fn resolve_handle(
        &self,
        session: SessionId,
        agent: AgentId,
        handle: MemoryHandle,
    ) -> Result<Option<MemoryId>, MemoryError> {
        let section = self.composed_section(session, agent).await?;
        Ok(section.handles().resolve(handle))
    }

    async fn composed_section(
        &self,
        session: SessionId,
        agent: AgentId,
    ) -> Result<Arc<MemorySection>, MemoryError> {
        let store = self.memory_store.clone();
        self.session_cache
            .get_or_load(session, agent, || async move {
                let rows = store
                    .list(agent)
                    .await
                    .map_err(|e| MemoryError::Backend(e.to_string()))?;
                Ok::<_, MemoryError>(compose_memory_section(&rows))
            })
            .await
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
    async fn system_prompt(
        &self,
        session: SessionId,
        viewer: Participant,
    ) -> Result<Arc<str>, MemoryError> {
        // Workers only run for agent receivers; a Human viewer is a wiring bug.
        let agent_id = viewer.agent_id().ok_or_else(|| {
            MemoryError::Backend("system_prompt called with Human viewer; agent worker only".into())
        })?;
        let role = self
            .prompt_cache
            .get_or_load(agent_id, &self.agents)
            .await?;
        let memory_section = self.composed_section(session, agent_id).await?;

        let core = self.core.as_ref();
        let role_str = role.as_str();
        let memory_str = memory_section.text();
        let separator = if memory_str.is_empty() { "" } else { "\n" };

        let mut out = String::with_capacity(
            CORE_TAG_OPEN.len()
                + core.len()
                + CORE_TAG_CLOSE.len()
                + ROLE_TAG_OPEN.len()
                + role_str.len()
                + ROLE_TAG_CLOSE.len()
                + separator.len()
                + memory_str.len(),
        );
        out.push_str(CORE_TAG_OPEN);
        out.push_str(core);
        out.push_str(CORE_TAG_CLOSE);
        out.push_str(ROLE_TAG_OPEN);
        out.push_str(role_str);
        out.push_str(ROLE_TAG_CLOSE);
        out.push_str(separator);
        out.push_str(memory_str);

        Ok(Arc::from(out))
    }
}
