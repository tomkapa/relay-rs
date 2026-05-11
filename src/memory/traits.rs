use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use thiserror::Error;

use crate::agents::AgentStoreError;
use crate::runtime::RequestKind;
use crate::session::{SessionError, SessionId};
use crate::types::Participant;

#[derive(Debug, Error)]
pub enum MemoryError {
    #[error("memory backend error: {0}")]
    Backend(String),

    #[error("session lookup: {0}")]
    Session(#[from] SessionError),

    #[error("agent lookup: {0}")]
    Agent(#[from] AgentStoreError),
}

/// Provides per-turn context to the agent. Returns the system prompt
/// for the active turn mode (`kind`); the implementation selects the
/// right `<core>` block for the kind and composes it with the agent's
/// role + memory section.
///
/// `viewer` is the participant the worker is currently driving — for an
/// agent↔agent session it disambiguates which side's role prompt to
/// load.
#[async_trait]
pub trait Memory: Send + Sync + fmt::Debug {
    async fn system_prompt(
        &self,
        session: SessionId,
        viewer: Participant,
        kind: RequestKind,
    ) -> Result<Arc<str>, MemoryError>;
}

pub type SharedMemory = Arc<dyn Memory>;
