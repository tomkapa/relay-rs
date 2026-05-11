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
use crate::runtime::RequestKind;
use crate::session::SessionId;
use crate::types::Participant;

use super::composer::MemorySection;
use super::loader::MemorySectionLoader;
use super::traits::{Memory, MemoryError};
use super::types::{MemoryHandle, MemoryId};

/// Stable XML-ish tags wrapping each prompt section. Marked `pub` so
/// consumers (e.g. tests, docs) can assert on the wire format if they
/// need to.
pub const CORE_TAG_OPEN: &str = "<core>\n";
pub const CORE_TAG_CLOSE: &str = "\n</core>\n";
pub const ROLE_TAG_OPEN: &str = "<role>\n";
pub const ROLE_TAG_CLOSE: &str = "\n</role>";

/// The per-mode `<core>` strings the composition root configures.
///
/// One field per [`RequestKind`] — exhaustive by construction. Adding a
/// new `RequestKind` variant produces a compile error here, forcing the
/// composition root to supply a core for the new mode.
#[derive(Debug, Clone)]
pub struct ModeCores {
    pub normal: Arc<str>,
    pub reflection: Arc<str>,
    pub resolution: Arc<str>,
}

impl ModeCores {
    /// Pick the core string for a request kind. Exhaustive `match` so a
    /// new variant lights up here at compile time.
    #[must_use]
    pub fn for_kind(&self, kind: RequestKind) -> Arc<str> {
        match kind {
            RequestKind::Normal => self.normal.clone(),
            RequestKind::Reflection => self.reflection.clone(),
            RequestKind::Resolution => self.resolution.clone(),
        }
    }
}

/// Composite memory that assembles the system prompt from a per-mode
/// core, a per-agent role string fetched on demand, and a per-session
/// composed memory section.
///
/// `prompt_cache` and `loader` are cheap-clone handles — both hold
/// their own `Arc` state internally, so sharing across subsystems is
/// just a clone. The loader is the single point that builds composed
/// sections; the memory tool layer (`MemoryToolDeps`) takes the same
/// loader so handle resolution and prompt rendering can never diverge.
pub struct AgentMemory {
    agents: SharedAgentStore,
    prompt_cache: AgentPromptCache,
    loader: MemorySectionLoader,
    cores: ModeCores,
}

impl AgentMemory {
    #[must_use]
    pub fn new(
        agents: SharedAgentStore,
        prompt_cache: AgentPromptCache,
        loader: MemorySectionLoader,
        cores: ModeCores,
    ) -> Self {
        Self {
            agents,
            prompt_cache,
            loader,
            cores,
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
        self.loader.resolve_handle(session, agent, handle).await
    }

    async fn composed_section(
        &self,
        session: SessionId,
        agent: AgentId,
    ) -> Result<Arc<MemorySection>, MemoryError> {
        self.loader.load(session, agent).await
    }
}

impl std::fmt::Debug for AgentMemory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentMemory").finish_non_exhaustive()
    }
}

#[async_trait]
impl Memory for AgentMemory {
    async fn system_prompt(
        &self,
        session: SessionId,
        viewer: Participant,
        kind: RequestKind,
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

        let core_arc = self.cores.for_kind(kind);
        let core = core_arc.as_ref();
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
