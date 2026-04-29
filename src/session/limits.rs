//! Session-store invariants. CLAUDE.md §5: every container has an explicit upper bound.

/// Maximum number of messages stored in a single session.
///
/// Each turn produces 1 user message (the prompt or the tool results) plus 1 assistant
/// message; this caps a session at ~256 turns including pathological tool loops. Beyond
/// this we refuse to grow the history rather than letting one runaway session eat a host.
pub const MAX_MESSAGES_PER_SESSION: usize = 512;

/// Default cap on simultaneously-tracked sessions in the in-memory store. Operators
/// running long-lived processes should override this when constructing the store.
pub const MAX_SESSIONS_DEFAULT: usize = 1024;
