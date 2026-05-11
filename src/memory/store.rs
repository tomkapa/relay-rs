//! Storage seam for the memory subsystem (doc/memory.md §2.1).
//!
//! One transactional mutation function is the only path through which memory
//! changes — both the agent's tool calls (Phase 3) and the operator surface
//! (Phase 8) call into it. The trait keeps the worker pool, librarian, and
//! HTTP layer decoupled from Postgres so the rest of the runtime can be
//! exercised against an in-memory fake when integration tests do not want a
//! database.

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use thiserror::Error;

use crate::agents::AgentId;
use crate::provider::ProviderError;
use crate::runtime::PromptRequestId;
use crate::types::ParseError;

use super::limits::CONTRADICTION_REASON_MAX_BYTES;
use super::types::{
    MemoryContent, MemoryEventId, MemoryId, MemoryKind, MemoryState, MutationKind,
    MutationSourceKind,
};

/// All failure modes the memory store can surface. CLAUDE.md §12 — one
/// error type at the module boundary, every variant exhaustive at the call
/// site.
#[derive(Debug, Error)]
pub enum MemoryStoreError {
    /// Target memory id was not found in `agent_memories`. Update / forget
    /// hit a row that has already been forgotten or never existed.
    #[error("memory {id:?} not found")]
    NotFound { id: MemoryId },

    /// Target row exists but belongs to a different agent. Memory is
    /// per-agent and private (doc/memory.md §1.11) — a cross-agent edit is
    /// a programming bug, not a recoverable error.
    #[error("memory {id:?} does not belong to agent {agent:?}")]
    WrongAgent { id: MemoryId, agent: AgentId },

    /// Pinned-row protection. Agent-driven mutations cannot edit or forget
    /// pinned rows; the operator path bypasses this check.
    #[error("memory {id:?} is pinned and cannot be mutated by an agent")]
    PinnedImmutable { id: MemoryId },

    /// Boundary parsing failure for a derived input (content cap exceeded
    /// during a rebuild, malformed handle).
    #[error("parse: {0}")]
    Parse(#[from] ParseError),

    /// Underlying Postgres error — wrapped as opaque so callers do not
    /// pattern-match on driver internals.
    #[error("memory store db error: {0}")]
    Db(#[from] sqlx::Error),

    /// Embedding provider call failed during a mutation. `MemoryStore::apply`
    /// propagates this so a missing embedding never produces a row that
    /// retrieval cannot match (doc/memory.md §2.9 — failure handling).
    #[error("memory store embedding provider: {0}")]
    Provider(#[from] ProviderError),
}

/// Provenance attached to every mutation. `Turn` rides on the calling
/// prompt-request id (the agent that just spoke); `Operator` and
/// `Librarian` carry no detail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MutationSource {
    Turn(PromptRequestId),
    Operator,
    Librarian,
}

impl MutationSource {
    /// The kind label persisted on the journal row.
    #[must_use]
    pub const fn kind(&self) -> MutationSourceKind {
        match self {
            Self::Turn(_) => MutationSourceKind::Turn,
            Self::Operator => MutationSourceKind::Operator,
            Self::Librarian => MutationSourceKind::Librarian,
        }
    }

    /// Detail column — only `Turn` carries a request id.
    #[must_use]
    pub const fn turn_id(&self) -> Option<PromptRequestId> {
        match self {
            Self::Turn(id) => Some(*id),
            Self::Operator | Self::Librarian => None,
        }
    }
}

/// Input to [`MemoryStore::apply`]. The single shape every mutation flows
/// through; the variant determines which journal `mutation` label is
/// written and which materialized-row update runs.
///
/// `state` is honoured directly today; the lifecycle state machine
/// (doc/memory.md §1.7) will narrow agent-facing callers later.
#[derive(Debug, Clone)]
pub enum MemoryMutation {
    /// Mint a new memory row.
    Write {
        agent: AgentId,
        kind: MemoryKind,
        content: MemoryContent,
        state: MemoryState,
        pinned: bool,
        source: MutationSource,
    },
    /// Replace `content` on an existing memory.
    Update {
        agent: AgentId,
        target: MemoryId,
        content: MemoryContent,
        state: MemoryState,
        source: MutationSource,
        /// When `true`, the pinned-row protection is bypassed. Set only on
        /// the operator path.
        operator_override: bool,
    },
    /// Drop a memory row from the materialized view. The journal retains
    /// the event so the row can be replayed back into existence.
    Forget {
        agent: AgentId,
        target: MemoryId,
        source: MutationSource,
        operator_override: bool,
    },
}

impl MemoryMutation {
    /// Mutation kind — drives the journal `mutation` column.
    #[must_use]
    pub const fn mutation_kind(&self) -> MutationKind {
        match self {
            Self::Write { .. } => MutationKind::Write,
            Self::Update { .. } => MutationKind::Update,
            Self::Forget { .. } => MutationKind::Forget,
        }
    }

    /// Owning agent — the row's `agent_id`. The store cross-checks against
    /// the materialized row on update / forget so a misrouted call surfaces
    /// as [`MemoryStoreError::WrongAgent`] rather than corrupting state.
    #[must_use]
    pub const fn agent(&self) -> AgentId {
        match self {
            Self::Write { agent, .. } | Self::Update { agent, .. } | Self::Forget { agent, .. } => {
                *agent
            }
        }
    }

    /// Provenance attached to the journal row.
    #[must_use]
    pub const fn source(&self) -> MutationSource {
        match self {
            Self::Write { source, .. }
            | Self::Update { source, .. }
            | Self::Forget { source, .. } => *source,
        }
    }
}

/// Outcome of a successful [`MemoryStore::apply`] call. Carries enough
/// information for the caller to know which rows changed without reading
/// back the materialized view.
#[derive(Debug, Clone)]
pub struct MutationOutcome {
    /// The journal row written.
    pub event_id: MemoryEventId,
    /// The materialized row id touched. For `Write` this is the freshly
    /// minted id; for `Update` / `Forget` it is the targeted row.
    pub memory_id: MemoryId,
    /// Materialized row after the mutation. `None` for `Forget` because
    /// the row is gone from `agent_memories`.
    pub row: Option<MemoryRow>,
}

/// Snapshot of a single row in `agent_memories`.
///
/// `embedding` is `None` when the row was written before the embedding
/// provider was wired (Phase 9); `Some` once the embedding writer has
/// populated it. Retrieval paths (Phase 2 contextual layer, Phase 3
/// `recall`, Phase 6 librarian) skip rows whose embedding is `None`.
#[derive(Debug, Clone)]
pub struct MemoryRow {
    pub id: MemoryId,
    pub agent_id: AgentId,
    pub kind: MemoryKind,
    pub content: MemoryContent,
    pub state: MemoryState,
    pub pinned: bool,
    pub source_turn_id: Option<PromptRequestId>,
    pub created_at: DateTime<Utc>,
    pub last_validated_at: DateTime<Utc>,
    pub last_accessed_at: DateTime<Utc>,
    pub access_count: u64,
}

/// Snapshot of a single row in `memory_events`.
#[derive(Debug, Clone)]
pub struct MemoryEvent {
    pub id: MemoryEventId,
    pub agent_id: AgentId,
    pub mutation: MutationKind,
    pub target_memory_id: MemoryId,
    pub content_before: Option<MemoryContent>,
    pub content_after: Option<MemoryContent>,
    pub source: MutationSource,
    pub created_at: DateTime<Utc>,
}

/// Filter applied by [`MemoryStore::search_by_embedding`].
#[derive(Debug, Clone, Default)]
pub struct SearchFilter {
    pub kinds: Option<Vec<crate::memory::MemoryKind>>,
    pub min_state: Option<crate::memory::MemoryState>,
}

/// One result from an embedding search — the row plus its cosine
/// similarity score.
#[derive(Debug, Clone)]
pub struct ScoredMemoryRow {
    pub row: MemoryRow,
    /// Cosine similarity in `[-1, 1]`. Higher is closer.
    pub similarity: f32,
}

/// A pair of memories the librarian flagged as duplicates / contradicting.
#[derive(Debug, Clone)]
pub struct PairCandidate {
    pub a: MemoryRow,
    pub b: MemoryRow,
    pub similarity: f32,
}

/// Storage seam for the memory subsystem. Implementations must be
/// thread-safe.
///
/// The trait is intentionally narrow: every mutation flows through
/// [`Self::apply`]; everything else is read or replay. Phase 2's renderer
/// and Phase 3's tool layer talk to this trait, never to a concrete impl.
#[async_trait]
pub trait MemoryStore: fmt::Debug + Send + Sync {
    /// Apply one mutation. Appends a journal row and updates the
    /// materialized table in a single transaction. Every memory write in
    /// the system goes through this function.
    async fn apply(&self, mutation: MemoryMutation) -> Result<MutationOutcome, MemoryStoreError>;

    /// Snapshot every materialized row for `agent`, ordered by
    /// `created_at` ascending. Used by the renderer (Phase 2) and the
    /// operator audit endpoint (Phase 8).
    async fn list(&self, agent: AgentId) -> Result<Vec<MemoryRow>, MemoryStoreError>;

    /// Fetch a single materialized row. Returns `None` if the row has been
    /// forgotten or never existed.
    async fn get(&self, id: MemoryId) -> Result<Option<MemoryRow>, MemoryStoreError>;

    /// Snapshot every journal event for `agent`, ordered chronologically.
    /// Powers the operator audit endpoint and the rebuild path
    /// ([`Self::rebuild_materialized`]).
    async fn list_events(&self, agent: AgentId) -> Result<Vec<MemoryEvent>, MemoryStoreError>;

    /// Rebuild `agent_memories` for a single agent by replaying the
    /// journal end to end. Used by tests that assert the materialized view
    /// is a deterministic projection of the events log; the operator
    /// revert path (Phase 8) calls this after appending an inverse event.
    async fn rebuild_materialized(&self, agent: AgentId) -> Result<(), MemoryStoreError>;

    /// Top-K cosine-similarity search over `agent`'s memory embeddings.
    /// Implementations that do not have an embedding writer wired
    /// (`embedding` column NULL on every row) return an empty vector —
    /// the renderer (Phase 2) and `recall` tool (Phase 3) treat the empty
    /// result as a degraded contextual layer rather than an error.
    async fn search_by_embedding(
        &self,
        agent: AgentId,
        embedding: &[f32],
        k: usize,
        filter: SearchFilter,
    ) -> Result<Vec<ScoredMemoryRow>, MemoryStoreError>;

    /// Pairs of an agent's memories whose cosine similarity ≥ `threshold`,
    /// excluding (a, a) self-pairs. Used by the librarian's dedup and
    /// contradiction-detection sweep (Phase 6). Pairs are returned each
    /// time only once (canonical ordering by id).
    async fn similar_pairs(
        &self,
        agent: AgentId,
        threshold: f32,
        max_pairs: usize,
    ) -> Result<Vec<PairCandidate>, MemoryStoreError>;

    /// Apply decay rules: any non-pinned `Validated` row whose
    /// `last_validated_at` is older than `cutoff` gets demoted to `Held`
    /// via the journal. Returns the number of rows demoted.
    async fn decay_validated(
        &self,
        agent: AgentId,
        cutoff: DateTime<Utc>,
    ) -> Result<usize, MemoryStoreError>;

    /// Stamp an independent-signal validation for the memory.
    /// Promotes the row's state per the lifecycle rules (Tentative → Held
    /// on first validation; Held → Validated on second). The journal is
    /// updated to reflect any state change so replay is faithful.
    async fn record_validation(
        &self,
        agent: AgentId,
        memory: MemoryId,
        source: ValidationSource,
        detail: Option<&str>,
    ) -> Result<MemoryRow, MemoryStoreError>;

    /// Insert a librarian-detected contradiction event for the given
    /// pair. Returns the event id. Idempotent on `(memory_a, memory_b)`
    /// against currently-unresolved rows — duplicate insert returns the
    /// existing id.
    async fn record_contradiction(
        &self,
        agent: AgentId,
        a: MemoryId,
        b: MemoryId,
        reason: &str,
    ) -> Result<crate::memory::ContradictionEventId, MemoryStoreError>;

    /// List unresolved contradiction events for an agent, oldest first.
    async fn unresolved_contradictions(
        &self,
        agent: AgentId,
    ) -> Result<Vec<ContradictionEventRow>, MemoryStoreError>;

    /// Fetch one contradiction event by id. Returns `None` if not found
    /// or already resolved.
    async fn read_contradiction(
        &self,
        id: crate::memory::ContradictionEventId,
    ) -> Result<Option<ContradictionEventRow>, MemoryStoreError>;

    /// Mark a contradiction event resolved. The variant determines whether
    /// the close points at a journal event ([`ResolutionOutcome::Mutation`])
    /// or carries a free-text rationale ([`ResolutionOutcome::NoAction`]).
    /// Idempotent: the underlying `WHERE resolved_at IS NULL` guard means a
    /// second call against an already-closed row is a no-op rather than a
    /// clobber.
    async fn resolve_contradiction(
        &self,
        id: crate::memory::ContradictionEventId,
        outcome: ResolutionOutcome,
    ) -> Result<(), MemoryStoreError>;

    /// Force-evict the lowest-scoring non-pinned rows beyond `quota`.
    /// Returns ids of evicted memories. Used by the librarian (Phase 6).
    async fn evict_overflow(
        &self,
        agent: AgentId,
        quota: usize,
    ) -> Result<Vec<MemoryId>, MemoryStoreError>;

    /// Append an inverse-mutation event for the given journal id and
    /// rebuild the materialized row. Used by the operator revert path
    /// (Phase 8). Returns the materialized row after revert (None when
    /// the revert removed the row).
    async fn revert_event(
        &self,
        agent: AgentId,
        event: MemoryEventId,
    ) -> Result<Option<MemoryRow>, MemoryStoreError>;

    /// Toggle the pinned flag on a row. Operator-only; agent paths cannot
    /// reach here.
    async fn set_pinned(
        &self,
        agent: AgentId,
        memory: MemoryId,
        pinned: bool,
    ) -> Result<MemoryRow, MemoryStoreError>;

    /// Increment the access counter and bump `last_accessed_at` for the
    /// rows whose ids match. Bounded — `ids.len()` ≤ `MAX_MEMORIES_PER_AGENT`.
    /// Reading does NOT advance validation (doc/memory.md §1.7).
    async fn record_access(&self, ids: &[MemoryId]) -> Result<(), MemoryStoreError>;
}

/// One row in `contradiction_events`. Three valid shapes mirror the
/// `contradiction_events_resolved_consistent` CHECK in migration 7:
///
/// * pending — every resolution column is `None`.
/// * mutation close — `resolved_at` set, `resolution_event_id` set,
///   `resolution_reason` is `None`.
/// * no-action close — `resolved_at` set, `resolution_event_id` is `None`,
///   `resolution_reason` set.
#[derive(Debug, Clone)]
pub struct ContradictionEventRow {
    pub id: crate::memory::ContradictionEventId,
    pub agent_id: AgentId,
    pub memory_a: MemoryId,
    pub memory_b: MemoryId,
    pub reason: String,
    pub created_at: DateTime<Utc>,
    pub resolved_at: Option<DateTime<Utc>>,
    pub resolution_event_id: Option<MemoryEventId>,
    pub resolution_reason: Option<String>,
}

/// Free-text rationale persisted on a no-action contradiction close.
///
/// Smart constructor at the boundary (CLAUDE.md §1) so the column's length
/// invariant is encoded in the type — once you hold a [`ResolutionReason`],
/// it is known to be 1..=`CONTRADICTION_REASON_MAX_BYTES` bytes.
#[derive(Debug, Clone)]
pub struct ResolutionReason(String);

impl ResolutionReason {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn into_inner(self) -> String {
        self.0
    }
}

impl TryFrom<String> for ResolutionReason {
    type Error = ParseError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        let len = value.len();
        if len == 0 {
            return Err(ParseError::Empty {
                field: "resolution_reason",
            });
        }
        if len > CONTRADICTION_REASON_MAX_BYTES {
            return Err(ParseError::TooLong {
                field: "resolution_reason",
                max: CONTRADICTION_REASON_MAX_BYTES,
                got: len,
            });
        }
        Ok(Self(value))
    }
}

/// Outcome of a resolution turn — fed to [`MemoryStore::resolve_contradiction`].
///
/// The two variants map to the two non-pending shapes of the
/// `contradiction_events_resolved_consistent` CHECK constraint, so the
/// type system makes the invalid combination (event id AND reason, or
/// neither) unrepresentable.
#[derive(Debug, Clone)]
pub enum ResolutionOutcome {
    /// A mutation tool (`memory_update` / `memory_forget`) closed the pair
    /// inline; `event_id` points at the journal row that did it.
    Mutation(MemoryEventId),
    /// The resolution turn ended without mutating either memory; `reason`
    /// is the assistant's final text (truncated to the column cap).
    NoAction { reason: ResolutionReason },
}

/// Origin of a [`MemoryStore::record_validation`] call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValidationSource {
    /// Cross-session re-write: the librarian found the same content
    /// emerging in a different session.
    CrossSessionRewrite,
    /// External confirmation: an agent's own follow-up turn (recall +
    /// web_search + reply) confirmed the memory.
    ExternalConfirmation,
    /// Operator endorsement: a `manager_note` flagged the memory as
    /// validated.
    OperatorEndorsement,
}

impl ValidationSource {
    /// Wire label used in the column constraint.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CrossSessionRewrite => "cross_session_rewrite",
            Self::ExternalConfirmation => "external_confirmation",
            Self::OperatorEndorsement => "operator_endorsement",
        }
    }
}

/// Cheap-clone handle so collaborators can hold the store without a
/// generic parameter.
pub type SharedMemoryStore = Arc<dyn MemoryStore>;
