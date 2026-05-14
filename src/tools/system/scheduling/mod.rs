//! Scheduling tools — agent-driven scheduling of future wake-up turns.
//!
//! A scheduled task fires through the standard `Normal` queue path:
//! the `ScheduledTaskScheduler` enqueues a `prompt_requests` row with
//! `sender = Participant::Human`, `parent_session = None`, addressed to
//! the owning agent. The fired turn looks indistinguishable from a
//! human-initiated `POST /prompts` to the worker pool.
//!
//! Authorization model:
//!
//! 1. The caller must be an agent (`ToolCallContext.viewer.is_agent()`).
//!    Humans don't run tool calls; this is defence in depth.
//! 2. The caller's `agent_id` becomes the task's `owner_agent_id`.
//!    `list_scheduled_tasks` filters by it; `cancel_scheduled_task`
//!    refuses cross-owner mutation with `OwnerMismatch` → `InvalidInput`.
//!
//! Three tools, each in its own file:
//!
//! - [`ScheduleTaskTool`] (`schedule_task`) — register a future wake-up.
//! - [`ListScheduledTasksTool`] (`list_scheduled_tasks`) — list the
//!   caller's active tasks.
//! - [`CancelScheduledTaskTool`] (`cancel_scheduled_task`) — cancel by
//!   id, scoped to the caller.

mod cancel_scheduled_task;
mod list_scheduled_tasks;
mod schedule_task;

pub use cancel_scheduled_task::CancelScheduledTaskTool;
pub use list_scheduled_tasks::ListScheduledTasksTool;
pub use schedule_task::ScheduleTaskTool;

/// Supplementary `<core>` instructions for the scheduling tool surface.
///
/// Concatenated onto the base `Normal` system prompt at composition
/// time so the tool docs and the model-facing instructions can't drift
/// apart — they live in the same module.
pub const SCHEDULING_CORE_PROMPT_SUPPLEMENT: &str = "\n\
    \n\
    Scheduling — for tasks that should happen later or repeatedly:\n\
    \n\
    9. Use `schedule_task` to register a future wake-up. The system will \
    fire the prompt you supply at the chosen time as a fresh turn for \
    you, exactly as if a user had typed it. Two shapes are supported: \
    `once` (a specific moment) and `recurring` (a set of weekdays at a \
    time-of-day in an IANA timezone). When the user explicitly asks for \
    a schedule (\"every morning at 7am, …\", \"remind me tomorrow to \
    …\"), call the tool directly. When you think a schedule would help \
    but the user did not ask, propose it first and wait for consent — \
    do not silently schedule. Always confirm the timezone if the user \
    didn't say; do not assume. Use `list_scheduled_tasks` to recall \
    what is already scheduled before adding a duplicate, and \
    `cancel_scheduled_task` to remove a task by id. Each scheduled \
    fire arrives in a brand-new conversation thread; reply through \
    `send_message(receiver=Human, …)` like any normal turn.";
