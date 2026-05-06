//! Memory abstraction.
//!
//! Today: a single trait that produces the system prompt for an upcoming turn. Tomorrow:
//! semantic retrieval (RAG), procedural memory (tool catalogues per persona), episodic
//! summarisation. The agent only knows about [`Memory`] — adding any of those is one
//! more impl, not a new code path through `Agent::reply`.

mod agent;
mod r#static;
mod traits;

pub use agent::{AgentMemory, CORE_TAG_CLOSE, CORE_TAG_OPEN, ROLE_TAG_CLOSE, ROLE_TAG_OPEN};
pub use r#static::StaticMemory;
pub use traits::{Memory, MemoryError, SharedMemory};
