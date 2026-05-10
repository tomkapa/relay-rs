//! Bounded TTL cache for agent system prompts.
//!
//! Per-turn prompt assembly hits this cache first; a miss or an expired
//! entry falls through to [`AgentStore::read`]. Backed by the generic
//! [`BoundedTtlCache`] so the eviction / TTL machinery stays in one
//! place. Cache size is bounded
//! ([`crate::agents::limits::AGENT_PROMPT_CACHE_CAP`]) and entries
//! expire after [`crate::agents::limits::AGENT_PROMPT_CACHE_TTL`] so an
//! admin's edit to an agent row becomes visible to live workers within
//! the TTL window.

use std::time::Duration;

use crate::cache::BoundedTtlCache;
use crate::clock::SharedClock;

use super::error::AgentStoreError;
use super::store::SharedAgentStore;
use super::types::{AgentId, AgentSystemPrompt};

/// Bounded TTL cache keyed by [`AgentId`]. Cheap-clone is not supported
/// on purpose — share via `Arc<AgentPromptCache>` if multiple owners
/// are needed.
#[derive(Debug)]
pub struct AgentPromptCache {
    inner: BoundedTtlCache<AgentId, AgentSystemPrompt>,
}

impl AgentPromptCache {
    #[must_use]
    pub fn new(cap: usize, ttl: Duration, clock: SharedClock) -> Self {
        Self {
            inner: BoundedTtlCache::new(cap, ttl, clock, "AgentPromptCache"),
        }
    }

    /// Return the cached prompt for `id`, refreshing from `store` on
    /// miss or expiry. The lock is released before the store call so a
    /// slow database does not block other workers.
    pub async fn get_or_load(
        &self,
        id: AgentId,
        store: &SharedAgentStore,
    ) -> Result<AgentSystemPrompt, AgentStoreError> {
        self.inner
            .get_or_load(id, || async move {
                let record = store.read(id).await?;
                Ok::<_, AgentStoreError>(record.system_prompt)
            })
            .await
    }
}
