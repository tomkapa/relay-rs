//! Memory abstraction.
//!
//! Today: a single trait that produces the system prompt for an upcoming turn. Tomorrow:
//! semantic retrieval (RAG), procedural memory (tool catalogues per persona), episodic
//! summarisation. The agent only knows about [`Memory`] — adding any of those is one
//! more impl, not a new code path through `Agent::reply`.

mod r#static;
mod traits;

pub use r#static::StaticMemory;
pub use traits::{Memory, MemoryError, SharedMemory};
