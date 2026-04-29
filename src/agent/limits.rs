//! Agent invariants. Per CLAUDE.md §5: every limit is named, doc-commented, and
//! exported so the operator can audit them in one place.

use std::time::Duration;

/// Default model output budget per turn. Comfortably under typical model caps; bumped
/// per-Agent via the builder when a tool-heavy task warrants it.
pub const DEFAULT_MAX_OUTPUT_TOKENS: u32 = 4096;

/// Default tool/turn iterations per `Agent::reply`. Above this the agent gives up rather
/// than letting a model loop on a stuck plan.
pub const DEFAULT_MAX_TURNS: u32 = 12;

/// Hard cap on tool calls inside a single assistant turn. Defends against a model that
/// fans out an unreasonable number of parallel calls.
pub const MAX_TOOL_CALLS_PER_TURN: usize = 16;

/// Per-call timeout for `LlmProvider::send`. CLAUDE.md §5: every I/O await is wrapped.
pub const PROVIDER_CALL_TIMEOUT: Duration = Duration::from_secs(120);

/// Per-call timeout for any `Tool::execute`. The tool may have its own narrower timeout
/// (e.g. fetch is 20 s); this is the outer fence.
pub const TOOL_CALL_TIMEOUT: Duration = Duration::from_secs(60);
