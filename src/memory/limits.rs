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

/// Byte budget for the rendered stable layer (pinned + Self) inside the
/// system prompt.
///
/// Sized so a few dozen one-or-two-sentence memories fit without crowding
/// the model's context window. Matches the order of magnitude of an
/// agent's role prompt, so doubling memory does not overwhelm role.
pub const STABLE_LAYER_MAX_BYTES: usize = 4096;

/// Byte budget for the rendered contextual layer (Other / Procedure /
/// Open).
///
/// The contextual layer is per-session, retrieved against the opener;
/// sized smaller than the stable layer because retrieval already narrows
/// the set.
pub const CONTEXTUAL_LAYER_MAX_BYTES: usize = 4096;

/// Top-K cap on the contextual retrieval. Phase 9 will rank by similarity
/// × recency × state and stop here.
pub const CONTEXTUAL_TOP_K: usize = 16;

/// Per-line overhead added to a memory's content when estimating render
/// size for the byte budget — covers `- [M-NN, validated] ` plus the
/// trailing newline. A worst-case estimate (handle up to four digits,
/// state label up to nine chars) so trimming never overshoots.
pub(super) const RENDER_LINE_OVERHEAD_BYTES: usize = 32;

/// In-process bound on the per-session memory composition cache.
///
/// Same rationale as `MAX_THREAD_SLOTS` in `runtime::limits`: caps memory
/// growth across a long-lived process. Eviction prefers the oldest
/// expired entry, falling back to the absolute oldest.
pub const SESSION_MEMORY_CACHE_CAP: usize = 1024;

/// TTL on the per-session memory composition cache.
///
/// Long enough that within an active session burst the assembled prompt
/// stays cached (frozen-for-session, doc/memory.md §1.3). Short enough
/// that a session resumed after a long idle picks up new memories
/// written in between.
pub const SESSION_MEMORY_CACHE_TTL_SECS: u64 = 60 * 60;

/// Per-turn cap on memory mutations (write + update + forget combined).
///
/// CLAUDE.md §5: every batch capped. A single normal turn that produces
/// more than this many memory mutations is almost certainly a model
/// runaway — we reject the overflowing call and let the model continue.
/// Reflection turns share the same per-turn cap.
pub const MAX_MEMORY_MUTATIONS_PER_TURN: usize = 8;

/// Per-reflection cap on total mutations.
///
/// doc/memory.md §1.6 — "dreams cannot produce 30 writes a night and
/// bloat the journal with noise". Higher than the per-turn cap because
/// reflection looks at a longer span of conversation, but still bounded.
pub const MAX_MEMORY_MUTATIONS_PER_REFLECTION: usize = 16;

/// Idle timeout after which a session qualifies for a reflection sweep
/// (doc/memory.md §1.6 — "the time since the last turn exceeds a
/// configurable idle timeout").
///
/// Long enough that an active conversation does not get reflected mid-
/// burst; short enough that overnight idleness produces consolidation
/// before the user resumes.
pub const REFLECTION_IDLE_TIMEOUT_SECS: u64 = 30 * 60;

/// Polling cadence for the reflection scheduler. Bounded so a tight
/// loop cannot pin the database under quiescence.
pub const REFLECTION_SCHEDULER_POLL_SECS: u64 = 60;

/// Cap on how many `(agent, session)` pairs the reflection scheduler
/// enqueues per poll. Bounded so a sudden burst of idle sessions cannot
/// overwhelm the queue.
pub const REFLECTION_SCHEDULER_BATCH_LIMIT: usize = 32;
