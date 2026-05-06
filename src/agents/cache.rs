//! Bounded TTL cache for agent system prompts.
//!
//! Per-turn prompt assembly hits this cache first; a miss or an expired entry
//! falls through to [`AgentStore::read`]. The cache is bounded
//! ([`crate::agents::limits::AGENT_PROMPT_CACHE_CAP`]) and entries expire after
//! [`crate::agents::limits::AGENT_PROMPT_CACHE_TTL`] so an admin's edit to an
//! agent row becomes visible to live workers within the TTL window.
//!
//! Hand-rolled rather than pulling in the `lru` crate (CLAUDE.md §8 zero-dep
//! bias). The hot path is one mutex round-trip + one HashMap lookup; on miss we
//! drop the lock, hit the store, then re-acquire to insert.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::clock::SharedClock;

use super::error::AgentStoreError;
use super::store::SharedAgentStore;
use super::types::{AgentId, AgentSystemPrompt};

#[derive(Debug, Clone)]
struct Entry {
    value: AgentSystemPrompt,
    fetched_at: Instant,
}

/// Bounded TTL cache keyed by [`AgentId`]. Cheap-clone is not supported on
/// purpose — share via `Arc<AgentPromptCache>` if multiple owners are needed.
pub struct AgentPromptCache {
    inner: Mutex<HashMap<AgentId, Entry>>,
    cap: usize,
    ttl: Duration,
    clock: SharedClock,
}

impl AgentPromptCache {
    #[must_use]
    pub fn new(cap: usize, ttl: Duration, clock: SharedClock) -> Self {
        // §6: zero-cap or zero-TTL would silently disable the cache; assert the
        // caller passes load-bearing values.
        assert!(cap > 0, "invariant: AgentPromptCache cap must be > 0");
        assert!(
            !ttl.is_zero(),
            "invariant: AgentPromptCache ttl must be > 0"
        );
        Self {
            inner: Mutex::new(HashMap::new()),
            cap,
            ttl,
            clock,
        }
    }

    /// Return the cached prompt for `id`, refreshing from `store` on miss or
    /// expiry. The lock is released before the store call so a slow database
    /// does not block other workers.
    pub async fn get_or_load(
        &self,
        id: AgentId,
        store: &SharedAgentStore,
    ) -> Result<AgentSystemPrompt, AgentStoreError> {
        let now = self.clock.now();

        if let Some(value) = self.lookup_fresh(id, now) {
            return Ok(value);
        }

        // Cache miss or expired — fetch under no lock.
        let record = store.read(id).await?;
        let value = record.system_prompt.clone();
        self.insert(id, value.clone(), now);
        Ok(value)
    }

    fn lookup_fresh(&self, id: AgentId, now: Instant) -> Option<AgentSystemPrompt> {
        let cache = self
            .inner
            .lock()
            .expect("invariant: AgentPromptCache mutex never poisoned");
        let entry = cache.get(&id)?;
        if now.saturating_duration_since(entry.fetched_at) >= self.ttl {
            return None;
        }
        Some(entry.value.clone())
    }

    fn insert(&self, id: AgentId, value: AgentSystemPrompt, now: Instant) {
        let mut cache = self
            .inner
            .lock()
            .expect("invariant: AgentPromptCache mutex never poisoned");
        if cache.len() >= self.cap && !cache.contains_key(&id) {
            evict_one(&mut cache, now, self.ttl);
            // §6: post-condition — eviction either freed a slot or the cache is
            // now under cap. If it isn't, every entry is a fresh duplicate and
            // we'd grow unbounded; assert the invariant before insert.
            assert!(
                cache.len() < self.cap,
                "invariant: AgentPromptCache eviction made room"
            );
        }
        cache.insert(
            id,
            Entry {
                value,
                fetched_at: now,
            },
        );
    }
}

impl std::fmt::Debug for AgentPromptCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentPromptCache")
            .field("cap", &self.cap)
            .field("ttl", &self.ttl)
            .finish_non_exhaustive()
    }
}

/// Evict the oldest expired entry, falling back to the absolute oldest when
/// nothing has expired. Bounded scan over `cap` entries.
fn evict_one(cache: &mut HashMap<AgentId, Entry>, now: Instant, ttl: Duration) {
    let mut oldest_expired: Option<AgentId> = None;
    let mut oldest_overall: Option<(AgentId, Instant)> = None;
    for (id, entry) in cache.iter() {
        if now.saturating_duration_since(entry.fetched_at) >= ttl {
            oldest_expired = Some(*id);
            break;
        }
        match oldest_overall {
            None => oldest_overall = Some((*id, entry.fetched_at)),
            Some((_, ts)) if entry.fetched_at < ts => {
                oldest_overall = Some((*id, entry.fetched_at));
            }
            _ => {}
        }
    }
    let victim = oldest_expired.or_else(|| oldest_overall.map(|(id, _)| id));
    if let Some(id) = victim {
        cache.remove(&id);
    }
}
