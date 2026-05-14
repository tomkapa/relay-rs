//! System tools — first-party capabilities the agent invokes through the
//! tool seam.
//!
//! Three families:
//!
//! * **Communication** — [`SendMessageTool`] (the only delivery mechanism
//!   for messages between participants) and [`GetSessionTool`]
//!   (cross-session read scoped to the caller's DAG). Both consume
//!   [`super::ToolCallContext`].
//! * **Memory** — [`MemoryWriteTool`], [`MemoryUpdateTool`],
//!   [`MemoryForgetTool`], [`MemoryValidateTool`], [`RecallTool`]. The
//!   four journal-writing tools share a per-turn cap via
//!   [`MemoryToolDeps`]; `recall` carries its own.
//! * **Built-in capabilities** — [`WebFetchTool`] and [`WebSearchTool`].
//!
//! Registration lives in the composition root (`src/app.rs`) — there is
//! no `register` helper here. Adding a system tool is one new file in
//! this directory + one `.with(...)` line in `app.rs`. Externally-supplied
//! tools enter through the MCP registry instead of this module.

mod get_session;
mod memory;
mod send_message;
mod web_fetch;
mod web_search;

pub use get_session::GetSessionTool;
pub use memory::{
    MemoryForgetTool, MemoryToolDeps, MemoryUpdateTool, MemoryValidateTool, MemoryWriteTool,
    RecallTool,
};
pub use send_message::SendMessageTool;
pub use web_fetch::WebFetchTool;
pub use web_search::WebSearchTool;
