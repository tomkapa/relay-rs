use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::provider::ChatMessage;

use super::error::SessionError;
use super::limits::{MAX_MESSAGES_PER_SESSION, MAX_SESSIONS_DEFAULT};

/// Opaque session identifier. Constructed by the store, opaque to callers.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct SessionId(Uuid);

impl SessionId {
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    #[must_use]
    pub fn as_uuid(self) -> Uuid {
        self.0
    }
}

impl Default for SessionId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("SessionId").field(&self.0).finish()
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// Storage trait for conversation history. Implementations must be thread-safe.
///
/// The `snapshot` method intentionally returns owned `Vec<ChatMessage>` rather than a
/// borrow — every concrete backend (Postgres, Redis, S3) needs to allocate anyway, and
/// the caller (the agent) consumes the snapshot when building the next request.
#[async_trait]
pub trait SessionStore: Send + Sync {
    async fn create(&self) -> Result<SessionId, SessionError>;
    async fn append(&self, id: SessionId, message: ChatMessage) -> Result<(), SessionError>;
    async fn snapshot(&self, id: SessionId) -> Result<Vec<ChatMessage>, SessionError>;
    async fn delete(&self, id: SessionId) -> Result<(), SessionError>;
}

/// Cheap-clone handle so `Agent` can hold the store without a generic parameter.
#[derive(Clone)]
pub struct SharedSessionStore(Arc<dyn SessionStore>);

impl SharedSessionStore {
    #[must_use]
    pub fn new(store: Arc<dyn SessionStore>) -> Self {
        Self(store)
    }

    pub async fn create(&self) -> Result<SessionId, SessionError> {
        self.0.create().await
    }

    pub async fn append(&self, id: SessionId, message: ChatMessage) -> Result<(), SessionError> {
        self.0.append(id, message).await
    }

    pub async fn snapshot(&self, id: SessionId) -> Result<Vec<ChatMessage>, SessionError> {
        self.0.snapshot(id).await
    }

    pub async fn delete(&self, id: SessionId) -> Result<(), SessionError> {
        self.0.delete(id).await
    }
}

impl fmt::Debug for SharedSessionStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("SharedSessionStore").finish()
    }
}

/// Process-local session storage. Suitable for the REPL and for tests; replace with a
/// durable backend when persistence matters.
pub struct InMemorySessionStore {
    sessions: RwLock<HashMap<SessionId, Vec<ChatMessage>>>,
    session_cap: usize,
    message_cap: usize,
}

impl InMemorySessionStore {
    #[must_use]
    pub fn new() -> Self {
        Self::with_caps(MAX_SESSIONS_DEFAULT, MAX_MESSAGES_PER_SESSION)
    }

    #[must_use]
    pub fn with_caps(session_cap: usize, message_cap: usize) -> Self {
        Self {
            sessions: RwLock::new(HashMap::new()),
            session_cap,
            message_cap,
        }
    }
}

impl Default for InMemorySessionStore {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for InMemorySessionStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InMemorySessionStore")
            .field("session_cap", &self.session_cap)
            .field("message_cap", &self.message_cap)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl SessionStore for InMemorySessionStore {
    async fn create(&self) -> Result<SessionId, SessionError> {
        let mut guard = self.sessions.write().await;
        if guard.len() >= self.session_cap {
            return Err(SessionError::SessionCapExceeded {
                max: self.session_cap,
            });
        }
        let id = SessionId::new();
        let prev = guard.insert(id, Vec::new());
        // §6: collisions are astronomically unlikely with v4 UUIDs — assert anyway,
        // because if we ever swap UUID generation for a counter, this catches it.
        assert!(prev.is_none(), "SessionId collision");
        Ok(id)
    }

    async fn append(&self, id: SessionId, message: ChatMessage) -> Result<(), SessionError> {
        let mut guard = self.sessions.write().await;
        let history = guard.get_mut(&id).ok_or(SessionError::NotFound(id))?;
        if history.len() >= self.message_cap {
            return Err(SessionError::MessageCapExceeded {
                id,
                max: self.message_cap,
            });
        }
        history.push(message);
        Ok(())
    }

    async fn snapshot(&self, id: SessionId) -> Result<Vec<ChatMessage>, SessionError> {
        let guard = self.sessions.read().await;
        let history = guard.get(&id).ok_or(SessionError::NotFound(id))?;
        Ok(history.clone())
    }

    async fn delete(&self, id: SessionId) -> Result<(), SessionError> {
        let mut guard = self.sessions.write().await;
        if guard.remove(&id).is_none() {
            return Err(SessionError::NotFound(id));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::UserContent;

    #[tokio::test]
    async fn create_append_snapshot_roundtrip() {
        let store = InMemorySessionStore::new();
        let id = store.create().await.expect("create");
        store
            .append(id, ChatMessage::User(vec![UserContent::Text("hi".into())]))
            .await
            .expect("append");
        let snap = store.snapshot(id).await.expect("snapshot");
        assert_eq!(snap.len(), 1);
    }

    #[tokio::test]
    async fn enforces_message_cap() {
        let store = InMemorySessionStore::with_caps(8, 2);
        let id = store.create().await.expect("create");
        for _ in 0..2 {
            store
                .append(id, ChatMessage::User(vec![UserContent::Text("x".into())]))
                .await
                .expect("under cap");
        }
        let err = store
            .append(
                id,
                ChatMessage::User(vec![UserContent::Text("over".into())]),
            )
            .await
            .expect_err("at cap");
        assert!(matches!(err, SessionError::MessageCapExceeded { .. }));
    }

    #[tokio::test]
    async fn enforces_session_cap() {
        let store = InMemorySessionStore::with_caps(1, 8);
        store.create().await.expect("first");
        let err = store.create().await.expect_err("second");
        assert!(matches!(err, SessionError::SessionCapExceeded { .. }));
    }

    #[tokio::test]
    async fn missing_session_is_not_found() {
        let store = InMemorySessionStore::new();
        let id = SessionId::new();
        let err = store.snapshot(id).await.expect_err("absent");
        assert!(matches!(err, SessionError::NotFound(_)));
    }
}
