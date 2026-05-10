//! Per-session cache of composed memory sections.
//!
//! Backed by the generic [`BoundedTtlCache`]; this module wires the
//! `(SessionId, AgentId)` key and the `Arc<MemorySection>` value
//! together with the loader closure the agent layer hands in.
//!
//! ## Divergence from `doc/memory.md` §1.3 — revisit
//!
//! The doc says memory is "assembled at session start and frozen for
//! the session's lifetime". The strict reading: store the composed
//! section as session state (e.g. `session_memory_snapshots` row at
//! create-time) so the snapshot survives process restarts and outlives
//! any cache eviction.
//!
//! What we ship instead: a process-local TTL cache. Composition is
//! lazy on the first turn that needs it; the section is held until TTL
//! eviction or process restart. This trades the strict "frozen for
//! session's lifetime" invariant for a much smaller diff (no schema /
//! write path / migration). The user-visible difference: a session
//! that lives longer than [`crate::memory::SESSION_MEMORY_CACHE_TTL_SECS`],
//! or that survives a process restart, will recompose its memory
//! section and pick up writes that landed in between turns.
//!
//! When to revisit: if drift across a long-lived session becomes a
//! correctness issue (e.g. an operator note shows up mid-conversation
//! and confuses the model), promote this to a `session_memory_snapshots`
//! table so the invariant is structurally enforced rather than
//! cache-luck.

use std::sync::Arc;
use std::time::Duration;

use crate::agents::AgentId;
use crate::cache::BoundedTtlCache;
use crate::clock::SharedClock;
use crate::session::SessionId;

use super::composer::MemorySection;

/// Per (session, agent) cache of composed sections. Cheap-clone is not
/// supported on purpose — share via `Arc<SessionMemoryCache>` if
/// multiple owners are needed.
#[derive(Debug)]
pub struct SessionMemoryCache {
    inner: BoundedTtlCache<(SessionId, AgentId), Arc<MemorySection>>,
}

impl SessionMemoryCache {
    #[must_use]
    pub fn new(cap: usize, ttl: Duration, clock: SharedClock) -> Self {
        Self {
            inner: BoundedTtlCache::new(cap, ttl, clock, "SessionMemoryCache"),
        }
    }

    /// Return the cached section for `(session, agent)`, calling
    /// `compose` to produce one on miss or expiry. The lock is released
    /// before `compose` runs so a slow store does not block other
    /// workers.
    pub async fn get_or_load<F, Fut, E>(
        &self,
        session: SessionId,
        agent: AgentId,
        compose: F,
    ) -> Result<Arc<MemorySection>, E>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<MemorySection, E>>,
    {
        self.inner
            .get_or_load((session, agent), || async move {
                let composed = compose().await?;
                Ok::<_, E>(Arc::new(composed))
            })
            .await
    }
}
