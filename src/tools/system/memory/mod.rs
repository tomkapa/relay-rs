//! Memory mutation tools (doc/memory.md §1.5, §2.3 — Phase 3).
//!
//! Three tools share a per-turn mutation counter so a single turn cannot
//! exceed [`MAX_MEMORY_MUTATIONS_PER_TURN`] across them combined:
//!
//! - [`MemoryWriteTool`] (`memory_write`) — mints a new memory at
//!   `Tentative`.
//! - [`MemoryUpdateTool`] (`memory_update`) — replaces content; resets
//!   state to `Tentative`. Pinned rows reject agent edits.
//! - [`MemoryForgetTool`] (`memory_forget`) — drops the materialized
//!   row; the journal keeps the event so reverts (Phase 8) work.
//!   Pinned rows reject agent forgets.
//!
//! `recall` is deferred to Phase 9 (needs the embedding provider).
//!
//! Each tool lives in its own file. Shared infrastructure
//! ([`MemoryToolDeps`] + the per-turn counter + handle resolution) lives
//! here in the module root.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tracing::warn;

use crate::agents::AgentId;
use crate::memory::{
    MAX_MEMORY_MUTATIONS_PER_TURN, MemoryHandle, MemoryId, MemoryStoreError, SessionMemoryCache,
    SharedMemoryStore, compose_memory_section,
};
use crate::runtime::PromptRequestId;
use crate::session::SessionId;
use crate::types::ParseError;

use super::super::traits::{ToolCallContext, ToolError};

mod forget;
mod update;
mod write;

pub use forget::MemoryForgetTool;
pub use update::MemoryUpdateTool;
pub use write::MemoryWriteTool;

/// Per-turn mutation counter shared by the three tools.
///
/// Bounded HashMap keyed by request id — when a turn exceeds the cap,
/// further mutation calls return `InvalidInput`. Entries are evicted in
/// bulk once the map is full so memory cannot grow without bound across
/// long-running processes.
#[derive(Debug)]
pub(super) struct MutationCounter {
    inner: Mutex<HashMap<PromptRequestId, usize>>,
    cap_per_turn: usize,
    bookkeeping_max_entries: usize,
}

impl MutationCounter {
    fn new() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
            cap_per_turn: MAX_MEMORY_MUTATIONS_PER_TURN,
            // Sized so a busy steady state of in-flight turns each
            // counting toward the cap fits without churn; bulk-evicted
            // when the map fills up.
            bookkeeping_max_entries: 1024,
        }
    }

    fn try_increment(&self, request_id: PromptRequestId) -> Result<usize, CapExceeded> {
        let mut map = self
            .inner
            .lock()
            .expect("invariant: MutationCounter mutex never poisoned");
        if map.len() >= self.bookkeeping_max_entries && !map.contains_key(&request_id) {
            // Bulk-clear when the map fills up. Counts older than this
            // bookkeeping bound were already enforced live; clearing
            // does not retroactively let a turn exceed the cap.
            map.clear();
        }
        let entry = map.entry(request_id).or_insert(0);
        if *entry >= self.cap_per_turn {
            return Err(CapExceeded {
                cap: self.cap_per_turn,
            });
        }
        *entry += 1;
        Ok(*entry)
    }
}

#[derive(Debug)]
pub(super) struct CapExceeded {
    pub cap: usize,
}

/// Shared infrastructure the three tools hold a handle on — the store,
/// the per-session handle map, and the per-turn counter.
#[derive(Debug, Clone)]
pub struct MemoryToolDeps {
    pub store: SharedMemoryStore,
    pub session_cache: SessionMemoryCache,
    pub(super) counter: Arc<MutationCounter>,
}

impl MemoryToolDeps {
    /// Construct from the storage seam + the session cache. The
    /// mutation counter is private to this module.
    #[must_use]
    pub fn new(store: SharedMemoryStore, session_cache: SessionMemoryCache) -> Self {
        Self {
            store,
            session_cache,
            counter: Arc::new(MutationCounter::new()),
        }
    }
}

pub(super) fn expect_agent(ctx: &ToolCallContext) -> Result<AgentId, ToolError> {
    ctx.viewer
        .agent_id()
        .ok_or_else(|| ToolError::Backend("memory tool invoked with non-agent viewer".into()))
}

pub(super) fn check_cap(
    counter: &MutationCounter,
    request_id: PromptRequestId,
) -> Result<(), ToolError> {
    counter.try_increment(request_id).map(|_| ()).map_err(|e| {
        ToolError::InvalidInput(format!(
            "memory mutation cap exceeded for this turn (max {} mutations)",
            e.cap
        ))
    })
}

pub(super) fn parse_to_tool_err(e: ParseError) -> ToolError {
    ToolError::InvalidInput(e.to_string())
}

pub(super) fn store_to_tool_err(e: MemoryStoreError) -> ToolError {
    match e {
        MemoryStoreError::NotFound { .. }
        | MemoryStoreError::WrongAgent { .. }
        | MemoryStoreError::PinnedImmutable { .. }
        | MemoryStoreError::Parse(_) => ToolError::InvalidInput(e.to_string()),
        MemoryStoreError::Db(_) => ToolError::Backend(e.to_string()),
    }
}

/// Resolve a session-scoped `M-NN` handle to its underlying memory id.
///
/// Composes the section through the session cache if it is not already
/// loaded — this is the same path the renderer takes, so a session
/// that just rolled past TTL pays one cache reload, not an error.
pub(super) async fn resolve_handle(
    deps: &MemoryToolDeps,
    session: SessionId,
    agent: AgentId,
    handle: MemoryHandle,
) -> Result<MemoryId, ToolError> {
    let store = deps.store.clone();
    let section = deps
        .session_cache
        .get_or_load(session, agent, || async move {
            let rows = store.list(agent).await.map_err(|e| {
                warn!(error = %e, relay.agent.id = %agent, "memory_tool.list.error");
                e
            })?;
            Ok::<_, MemoryStoreError>(compose_memory_section(&rows))
        })
        .await
        .map_err(store_to_tool_err)?;

    section.handles().resolve(handle).ok_or_else(|| {
        ToolError::InvalidInput(format!(
            "unknown memory handle {handle}; check the `## Memory` section in your system prompt for valid handles"
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counter_caps_at_per_turn_limit() {
        let counter = MutationCounter::new();
        let req = PromptRequestId::new();
        for i in 1..=MAX_MEMORY_MUTATIONS_PER_TURN {
            assert_eq!(counter.try_increment(req).expect("under cap"), i);
        }
        assert!(counter.try_increment(req).is_err());
    }

    #[test]
    fn counter_is_per_request() {
        let counter = MutationCounter::new();
        let r1 = PromptRequestId::new();
        let r2 = PromptRequestId::new();
        for _ in 0..MAX_MEMORY_MUTATIONS_PER_TURN {
            counter.try_increment(r1).expect("r1 ok");
        }
        // r2's quota is fresh.
        counter.try_increment(r2).expect("r2 ok");
    }
}
