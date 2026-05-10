//! Memory subsystem caps. CLAUDE.md §5 — every value is documented with *why
//! this number*.
//!
//! These are the storage-layer bounds Phase 1 enforces. Reflection /
//! resolution caps and per-agent quotas land in later phases (§2.4 / §2.6) and
//! will live next to those modules.

/// Maximum bytes of a single memory's `content`.
///
/// One or two sentences (doc/memory.md §1.2) — sized so a stable layer of
/// dozens of memories still fits comfortably under the model's prompt
/// budget. Mirrors the column `CHECK (octet_length(content) BETWEEN 1 AND
/// 4096)` in migration 7.
pub const MEMORY_CONTENT_MAX_BYTES: usize = 4096;

/// Maximum bytes of a `contradiction_events.reason` blurb.
///
/// Librarian-written, never agent-facing — sized so the heuristic that
/// flagged the pair (similarity score, opposing keywords) can be recorded
/// in human-readable form without bloating the table.
pub const CONTRADICTION_REASON_MAX_BYTES: usize = 1024;

/// Hard cap on the number of materialized memory rows held per agent.
///
/// CLAUDE.md §5: every batch capped. Keeps the renderer's stable +
/// contextual layer bounded (Phase 2) and gives the librarian a target
/// for eviction (Phase 6). Reads at this surface assert the cap as a
/// saturation signal — overshooting means the writer ignored the same
/// limit.
pub const MAX_MEMORIES_PER_AGENT: usize = 1024;

/// Hard cap on a single page of journal events returned to in-process
/// callers.
///
/// The journal grows unbounded with mutation history, so any `fetch_all`
/// over it needs an upper bound. Operator audit (Phase 8) will paginate
/// through this cap with a cursor.
pub const MAX_EVENTS_PER_PAGE: usize = 1024;
