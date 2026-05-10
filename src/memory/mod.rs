//! Memory abstraction.
//!
//! Two seams live side by side:
//!
//! - [`Memory`] — per-turn system-prompt assembly (`<core>` + `<role>`). The
//!   agent loop calls into this once per turn. Backed today by
//!   [`AgentMemory`] / [`StaticMemory`].
//! - [`MemoryStore`] — the agent's persistent, journaled memory rows
//!   (doc/memory.md). Phase 1 lands the storage foundation
//!   ([`PgMemoryStore`]); later phases add retrieval, reflection, and the
//!   librarian on top of the same trait.

mod agent;
mod limits;
mod pg_store;
mod r#static;
mod store;
mod traits;
mod types;

pub use agent::{AgentMemory, CORE_TAG_CLOSE, CORE_TAG_OPEN, ROLE_TAG_CLOSE, ROLE_TAG_OPEN};
pub use limits::{
    CONTRADICTION_REASON_MAX_BYTES, MAX_EVENTS_PER_PAGE, MAX_MEMORIES_PER_AGENT,
    MEMORY_CONTENT_MAX_BYTES,
};
pub use pg_store::PgMemoryStore;
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
