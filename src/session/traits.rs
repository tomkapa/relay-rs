//! Session storage trait + opaque [`SessionId`].
//!
//! The agent never holds a `Vec<ChatMessage>` directly — it asks a [`SessionStore`] for a
//! snapshot before each turn and appends new messages back. Persistence (today: Postgres
//! via [`super::PgSessionStore`]; tomorrow: anything else) is one more impl, not a new
//! agent code path.

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;

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
    async fn create(&self) -> Result<SessionId, SessionError>;
    async fn append(&self, id: SessionId, message: ChatMessage) -> Result<(), SessionError>;
    async fn snapshot(&self, id: SessionId) -> Result<Vec<ChatMessage>, SessionError>;
    async fn delete(&self, id: SessionId) -> Result<(), SessionError>;
}

/// Cheap-clone handle so `Agent` can hold the store without a generic parameter.
pub type SharedSessionStore = Arc<dyn SessionStore>;
