use thiserror::Error;

use super::store::SessionId;

#[derive(Debug, Error)]
pub enum SessionError {
    #[error("session {0:?} not found")]
    NotFound(SessionId),

    #[error("session {id:?} would exceed message cap ({max})")]
    MessageCapExceeded { id: SessionId, max: usize },

    #[error("session store reached its session cap ({max})")]
    SessionCapExceeded { max: usize },

    #[error("session store backend error: {0}")]
    Backend(String),
}
