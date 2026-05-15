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
//!
//! Cheap-clone — sharing the cache between subsystems does not need an
//! external `Arc<...>` wrapper.

use std::sync::Arc;
use std::time::Duration;

use crate::cache::BoundedTtlCache;
use crate::clock::SharedClock;

use super::error::AgentStoreError;
use super::store::SharedAgentStore;
use super::types::{AgentId, AgentName, AgentSystemPrompt};

/// Bounded TTL cache keyed by [`AgentId`]. Cheap-clone — the inner
/// [`BoundedTtlCache`] is itself an `Arc`, so cloning shares the
/// underlying state.
#[derive(Debug, Clone)]
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

/// Bounded TTL cache for the global agent name index.
///
/// Caches the `(id, name)` pairs that feed the `<agents>` block on every
/// turn. Single-slot (`cap = 1`) because the snapshot is global, not
/// keyed; same TTL as [`AgentPromptCache`] so an admin's rename / create
/// / delete becomes visible to live workers within the same window.
/// Cheap-clone — the inner `Arc<[..]>` makes the hot-path clone two
/// atomic increments, not a per-element copy.
#[derive(Debug, Clone)]
pub struct AgentNamesCache {
    inner: BoundedTtlCache<(), Arc<[(AgentId, AgentName)]>>,
}

impl AgentNamesCache {
    /// `ttl` should match [`AgentPromptCache`]'s TTL so the two surfaces
    /// share a single liveness window.
    #[must_use]
    pub fn new(ttl: Duration, clock: SharedClock) -> Self {
        Self {
            inner: BoundedTtlCache::new(1, ttl, clock, "AgentNamesCache"),
        }
    }

    /// Return the cached name index, refreshing from `store` on miss or
    /// expiry. The lock is released before the store call so a slow
    /// database does not block other workers.
    pub async fn get_or_load(
        &self,
        store: &SharedAgentStore,
    ) -> Result<Arc<[(AgentId, AgentName)]>, AgentStoreError> {
        self.inner
            .get_or_load((), || async move {
                let names = store.list_names().await?;
                Ok::<_, AgentStoreError>(Arc::from(names))
            })
            .await
    }
}
