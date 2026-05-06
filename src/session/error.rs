use thiserror::Error;

use crate::agents::AgentId;

use super::traits::SessionId;

#[derive(Debug, Error)]
pub enum SessionError {
    #[error("session {0:?} not found")]
    NotFound(SessionId),

    /// Session-create was given an `agent_id` that does not exist in the
    /// `agents` table. Distinct from [`SessionError::NotFound`] (about the
    /// session itself) so handlers can map it to a different status code.
    #[error("agent {0:?} not found")]
    AgentNotFound(AgentId),

    #[error("session {id:?} would exceed message cap ({max})")]
    MessageCapExceeded { id: SessionId, max: u32 },

    #[error("session store backend error: {0}")]
    Backend(String),

    #[error("session store db error: {0}")]
    Db(#[from] sqlx::Error),
}
