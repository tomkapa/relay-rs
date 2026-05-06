//! Bounds for the agents subsystem. CLAUDE.md §5: every limit is named, doc-commented,
//! and exported so the operator can audit them in one place.

use std::time::Duration;

/// Maximum length, in bytes, of an agent's display name. Mirrors the
/// `octet_length(name) BETWEEN 1 AND 64` check on the `agents` table.
pub const AGENT_NAME_MAX_LEN: usize = 64;

/// Maximum length, in bytes, of an agent's role-specific system prompt.
///
/// Mirrors the `octet_length(system_prompt) BETWEEN 1 AND 65536` check on the
/// `agents` table. Sized so the assembled `<core> + <role>` string still fits
/// comfortably within typical model context windows.
pub const AGENT_SYSTEM_PROMPT_MAX_LEN: usize = 64 * 1024;

/// Capacity of the per-worker [`crate::agents::AgentPromptCache`]. Sized so that
/// a deployment with hundreds of agents still fits without eviction churn while
/// staying small enough to bound memory.
pub const AGENT_PROMPT_CACHE_CAP: usize = 256;

/// TTL for cached agent prompts. Edits to an agent's `system_prompt` row become
/// visible to live workers within this window — no LISTEN/NOTIFY required.
pub const AGENT_PROMPT_CACHE_TTL: Duration = Duration::from_secs(60);
