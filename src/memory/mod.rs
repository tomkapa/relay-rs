//! Memory abstraction.
//!
//! Two seams live side by side:
//!
//! - [`Memory`] — per-turn system-prompt assembly (`<core>` + `<role>` +
//!   composed `<memory>` section). The agent loop calls into this once
//!   per turn. Backed today by [`AgentMemory`] / [`StaticMemory`].
//! - [`MemoryStore`] — the agent's persistent, journaled memory rows
//!   (doc/memory.md). Phase 1 lands the storage foundation
//!   ([`PgMemoryStore`]); Phase 2 lands the composer + session cache
//!   that turn rows into a system-prompt section.

mod agent;
mod composer;
mod limits;
mod pg_store;
mod reflection_scheduler;
mod session_cache;
mod r#static;
mod store;
mod traits;
mod types;

pub use agent::{AgentMemory, CORE_TAG_CLOSE, CORE_TAG_OPEN, ROLE_TAG_CLOSE, ROLE_TAG_OPEN};
pub use composer::{
    MEMORY_TAG_CLOSE, MEMORY_TAG_OPEN, MemoryHandleMap, MemorySection, compose_memory_section,
};
pub use limits::{
    CONTEXTUAL_LAYER_MAX_BYTES, CONTEXTUAL_TOP_K, CONTRADICTION_REASON_MAX_BYTES,
    MAX_EVENTS_PER_PAGE, MAX_MEMORIES_PER_AGENT, MAX_MEMORY_MUTATIONS_PER_REFLECTION,
    MAX_MEMORY_MUTATIONS_PER_TURN, MEMORY_CONTENT_MAX_BYTES, REFLECTION_IDLE_TIMEOUT_SECS,
    REFLECTION_SCHEDULER_BATCH_LIMIT, REFLECTION_SCHEDULER_POLL_SECS, SESSION_MEMORY_CACHE_CAP,
    SESSION_MEMORY_CACHE_TTL_SECS, STABLE_LAYER_MAX_BYTES,
};
pub use pg_store::PgMemoryStore;
pub use reflection_scheduler::ReflectionScheduler;
pub use session_cache::SessionMemoryCache;
pub use r#static::StaticMemory;
pub use store::{
    MemoryEvent, MemoryMutation, MemoryRow, MemoryStore, MemoryStoreError, MutationOutcome,
    MutationSource, SharedMemoryStore,
};
pub use traits::{Memory, MemoryError, SharedMemory};
pub use types::{
    ContradictionEventId, MemoryContent, MemoryEventId, MemoryHandle, MemoryId, MemoryKind,
    MemoryState, MutationKind, MutationSourceKind,
};
