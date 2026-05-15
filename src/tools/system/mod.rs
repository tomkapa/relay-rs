//! System tools — first-party capabilities the agent invokes through the
//! tool seam.
//!
//! Four families:
//!
//! * **Communication** — [`SendMessageTool`] (the only delivery mechanism
//!   for messages between participants) and [`GetSessionTool`]
//!   (cross-session read scoped to the caller's DAG). Both consume
//!   [`super::ToolCallContext`].
//! * **Memory** — [`MemoryWriteTool`], [`MemoryUpdateTool`],
//!   [`MemoryForgetTool`], [`MemoryValidateTool`], [`RecallTool`]. The
//!   four journal-writing tools share a per-turn cap via
//!   [`MemoryToolDeps`]; `recall` carries its own.
//! * **Scheduling** — [`ScheduleTaskTool`], [`ListScheduledTasksTool`],
//!   [`CancelScheduledTaskTool`]. Persist a future wake-up; the
//!   `ScheduledTaskScheduler` enqueues a `prompt_requests` row at fire
//!   time so the agent receives a fresh turn.
//! * **Built-in capabilities** — [`WebFetchTool`] and [`WebSearchTool`].
//!
//! Registration lives in the composition root (`src/app.rs`) — there is
//! no `register` helper here. Adding a system tool is one new file in
//! this directory + one `.with(...)` line in `app.rs`. Externally-supplied
//! tools enter through the MCP registry instead of this module.

mod get_session;
mod memory;
mod scheduling;
mod search_agents;
mod send_message;
mod web_fetch;
mod web_search;

pub use get_session::GetSessionTool;
pub use memory::{
    MemoryForgetTool, MemoryToolDeps, MemoryUpdateTool, MemoryValidateTool, MemoryWriteTool,
    RecallTool,
};
pub use scheduling::{
    CancelScheduledTaskTool, ListScheduledTasksTool, SCHEDULING_CORE_PROMPT_SUPPLEMENT,
    ScheduleTaskTool,
};
pub use search_agents::SearchAgentsTool;
pub use send_message::SendMessageTool;
pub use web_fetch::WebFetchTool;
pub use web_search::WebSearchTool;
