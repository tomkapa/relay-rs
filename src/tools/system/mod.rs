//! System tools — first-party capabilities the agent invokes through
//! the tool seam.
//!
//! Two flavours:
//!
//! * **Communication** — [`SendMessageTool`] (the only delivery
//!   mechanism for messages between participants) and [`GetSessionTool`]
//!   (cross-session read scoped to the caller's DAG). Both consume
//!   [`super::ToolCallContext`] via `execute_with_ctx`.
//! * **Memory** — [`MemoryWriteTool`], [`MemoryUpdateTool`],
//!   [`MemoryForgetTool`] (Phase 3). All three share a per-turn
//!   mutation counter via [`MemoryToolDeps`].
//! * **Built-in capabilities** — [`WebFetchTool`] and
//!   [`WebSearchTool`].
//!
//! Registration lives in the composition root (`src/app.rs`) — there
//! is no `register` helper here. Each tool's constructor is
//! `pub`-exported so adding a system tool is one new file in this
//! directory + one `.with(...)` line in `app.rs`. Externally-supplied
//! tools enter through the MCP registry instead of this module.

mod get_session;
mod memory;
mod send_message;
mod web_fetch;
mod web_search;

pub use get_session::GetSessionTool;
pub use memory::{MemoryForgetTool, MemoryToolDeps, MemoryUpdateTool, MemoryWriteTool};
pub use send_message::SendMessageTool;
pub use web_fetch::WebFetchTool;
pub use web_search::WebSearchTool;
