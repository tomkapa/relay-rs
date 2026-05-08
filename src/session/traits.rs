//! Session storage trait + opaque [`SessionId`].
//!
//! Sessions are 2-party (`Human ↔ Agent` or `Agent ↔ Agent`); a single
//! conversation between two parties. The agent never holds a `Vec<ChatMessage>`
//! directly — it asks a [`SessionStore`] for a viewer-scoped snapshot before
//! each turn and appends new messages back.

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;

use crate::provider::ChatMessage;
use crate::runtime::PromptRequestId;
use crate::types::{MessageSender, Participant};

use super::error::SessionError;

crate::uuid_newtype! {
    /// Opaque session identifier. Constructed by the store, opaque to callers.
    pub SessionId
}

/// Storage trait for conversation history. Implementations must be thread-safe.
///
/// Returns owned values rather than borrows — every backend (Postgres, Redis,
/// future S3) needs to allocate anyway and the caller (the agent) consumes
/// each result.
#[async_trait]
pub trait SessionStore: fmt::Debug + Send + Sync {
    /// Resolve or create the session bound to a canonical participant pair
    /// inside a DAG. Idempotent: two callers with the same `(root_request_id,
    /// canonical(a, b))` always reach the same session id. The store itself
    /// canonicalises the pair so a caller cannot accidentally insert the
    /// reversed-order row.
    ///
    /// `parent_session_id` is the session this conversation forks from (the
    /// caller's own session — `None` for the root human-to-agent session).
    async fn resolve_or_create_for_pair(
        &self,
        root_request_id: PromptRequestId,
        a: Participant,
        b: Participant,
        parent_session_id: Option<SessionId>,
    ) -> Result<SessionId, SessionError>;

    /// Append one chat message authored by `sender` and addressed to `receiver`.
    /// `sender == receiver` is rejected (self-messages are representationally
    /// invalid). The store does not check that `sender`/`receiver` are members
    /// of the session — the type-system invariant on `send_message` enforces
    /// that at the call site.
    async fn append(
        &self,
        id: SessionId,
        sender: MessageSender,
        receiver: Participant,
        message: ChatMessage,
    ) -> Result<(), SessionError>;

    /// Append a worker-injected `system`-kind nudge addressed to `receiver`.
    /// The body is wrapped as `ChatMessage::User(vec![Text(note)])` on the
    /// way in so the receiving agent renders it as user-side context.
    async fn append_system_nudge(
        &self,
        id: SessionId,
        receiver: Participant,
        note: String,
    ) -> Result<(), SessionError>;

    /// Render every message in `id` from `viewer`'s perspective:
    /// `sender == viewer` becomes `ChatMessage::Assistant`; everything else
    /// (including `system` rows) becomes `ChatMessage::User`. Used by
    /// `Agent::send_one_turn` to build the prompt without rebuilding history.
    async fn snapshot(
        &self,
        id: SessionId,
        viewer: Participant,
    ) -> Result<Vec<ChatMessage>, SessionError>;

    /// Paginated viewer-mapped snapshot. Returns up to `limit` messages
    /// ordered by `seq` ascending, taken from the most-recent end of the
    /// session — i.e. the rows with the largest `seq < before_seq` (or, when
    /// `before_seq` is `None`, the largest `seq`). Used by the `get_session`
    /// tool so the calling agent can pull a sibling's history without
    /// streaming the entire log.
    ///
    /// Each entry's `i64` is the row's `seq`; the smallest one is the next
    /// `before_seq` cursor for further pagination.
    async fn snapshot_window(
        &self,
        id: SessionId,
        viewer: Participant,
        limit: u32,
        before_seq: Option<i64>,
    ) -> Result<Vec<(i64, ChatMessage)>, SessionError>;

    /// Two participants of `id`. The pair is in canonical order so callers
    /// can pattern-match deterministically.
    async fn participants(&self, id: SessionId)
    -> Result<(Participant, Participant), SessionError>;

    /// `Some(parent)` when `id` was forked from another session by a
    /// `send_message` call; `None` for the human-to-agent root.
    async fn parent(&self, id: SessionId) -> Result<Option<SessionId>, SessionError>;

    /// Single-RTT: viewer-mapped history of `id`'s parent — but *only* when
    /// `viewer` is itself a participant of the parent. Otherwise (no parent;
    /// viewer not in parent's participants; parent empty) returns an empty
    /// vector. Called on every turn the agent loop runs; the default impl
    /// fans out to `parent` + `participants` + `snapshot` (three round-trips)
    /// so the optimization is opt-in per backend. The Postgres impl
    /// overrides with one query that joins through `sessions` to
    /// `session_messages`.
    async fn parent_history_for_viewer(
        &self,
        id: SessionId,
        viewer: Participant,
    ) -> Result<Vec<ChatMessage>, SessionError> {
        let Some(parent) = self.parent(id).await? else {
            return Ok(Vec::new());
        };
        let (a, b) = self.participants(parent).await?;
        if viewer != a && viewer != b {
            return Ok(Vec::new());
        }
        self.snapshot(parent, viewer).await
    }

    /// DAG anchor — the `prompt_requests.id` that rooted the conversation
    /// tree this session belongs to. Used by the `send_message` tool to
    /// resolve sibling sessions and by the DAG-budget bump.
    async fn root_request_id(&self, id: SessionId) -> Result<PromptRequestId, SessionError>;

    /// Drop the session and every message it owns.
    async fn delete(&self, id: SessionId) -> Result<(), SessionError>;
}

/// Cheap-clone handle so consumers can hold the store without a generic parameter.
pub type SharedSessionStore = Arc<dyn SessionStore>;
