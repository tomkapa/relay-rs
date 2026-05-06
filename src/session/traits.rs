//! Session storage trait + opaque [`SessionId`].
//!
//! The agent never holds a `Vec<ChatMessage>` directly — it asks a [`SessionStore`] for a
//! snapshot before each turn and appends new messages back. Persistence (today: Postgres
//! via [`super::PgSessionStore`]; tomorrow: anything else) is one more impl, not a new
//! agent code path.

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;

use crate::agents::AgentId;
use crate::provider::ChatMessage;

use super::error::SessionError;

crate::uuid_newtype! {
    /// Opaque session identifier. Constructed by the store, opaque to callers.
    pub SessionId
}

/// Storage trait for conversation history. Implementations must be thread-safe.
///
/// The `snapshot` method intentionally returns owned `Vec<ChatMessage>` rather than a
/// borrow — every concrete backend (Postgres, Redis, S3) needs to allocate anyway, and
/// the caller (the agent) consumes the snapshot when building the next request.
#[async_trait]
pub trait SessionStore: fmt::Debug + Send + Sync {
    /// Mint a new session bound to `agent_id`. The agent is fixed for the
    /// session's lifetime — every turn loads its system prompt from this id, so
    /// swapping mid-session would derail the model's behaviour. The HTTP layer
    /// resolves "no agent_id given" to the default agent before calling.
    async fn create(&self, agent_id: AgentId) -> Result<SessionId, SessionError>;
    async fn append(&self, id: SessionId, message: ChatMessage) -> Result<(), SessionError>;
    async fn snapshot(&self, id: SessionId) -> Result<Vec<ChatMessage>, SessionError>;
    /// Resolve the agent bound to `id`. Called by the memory layer at the start
    /// of every turn; complement to `snapshot`.
    async fn agent_id(&self, id: SessionId) -> Result<AgentId, SessionError>;
    async fn delete(&self, id: SessionId) -> Result<(), SessionError>;
}

/// Cheap-clone handle so `Agent` can hold the store without a generic parameter.
pub type SharedSessionStore = Arc<dyn SessionStore>;
