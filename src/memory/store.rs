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
use crate::runtime::PromptRequestId;
use crate::types::ParseError;

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
/// The embedding is omitted from this projection — Phase 1 has no
/// embedding writer, and the renderer (Phase 2) reads it through a
/// separate query when it needs the vector.
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
}

/// Cheap-clone handle so collaborators can hold the store without a
/// generic parameter.
pub type SharedMemoryStore = Arc<dyn MemoryStore>;
