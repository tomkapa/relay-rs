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

/// Capacity of the per-worker [`crate::agents::AgentPromptCache`].
///
/// Bounds the live working set in worker memory; the `agents` table itself is
/// unbounded (SaaS), and rare tenants whose agent isn't cached pay one DB read
/// per turn.
pub const AGENT_PROMPT_CACHE_CAP: usize = 256;

/// TTL for cached agent prompts. Edits to an agent's `system_prompt` row become
/// visible to live workers within this window — no LISTEN/NOTIFY required.
pub const AGENT_PROMPT_CACHE_TTL: Duration = Duration::from_secs(60);

/// Maximum number of MCP server ids that may sit in one agent's `allowed_mcp_servers` array.
///
/// Mirrors `crate::mcp::MAX_MCP_SERVERS`: an agent could legitimately be
/// granted every server registered system-wide, so a tighter per-agent cap
/// would just create a confusing asymmetry. The schema
/// `CHECK (cardinality(...) <= 32)` enforces the same number on the DB side.
pub const MAX_ALLOWED_MCP_SERVERS_PER_AGENT: usize = 32;
