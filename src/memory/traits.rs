use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use thiserror::Error;

use crate::agents::AgentStoreError;
use crate::session::{SessionError, SessionId};

#[derive(Debug, Error)]
pub enum MemoryError {
    #[error("memory backend error: {0}")]
    Backend(String),

    #[error("session lookup: {0}")]
    Session(#[from] SessionError),

    #[error("agent lookup: {0}")]
    Agent(#[from] AgentStoreError),
}

/// Provides per-turn context to the agent. Returns the system prompt; future revisions
/// can extend with retrieval results, persona blocks, etc.
#[async_trait]
pub trait Memory: Send + Sync + fmt::Debug {
    async fn system_prompt(&self, session: SessionId) -> Result<Arc<str>, MemoryError>;
}

pub type SharedMemory = Arc<dyn Memory>;
