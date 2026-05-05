use thiserror::Error;

use super::traits::SessionId;

#[derive(Debug, Error)]
pub enum SessionError {
    #[error("session {0:?} not found")]
    NotFound(SessionId),

    #[error("session {id:?} would exceed message cap ({max})")]
    MessageCapExceeded { id: SessionId, max: u32 },

    #[error("session store backend error: {0}")]
    Backend(String),

    #[error("session store db error: {0}")]
    Db(#[from] sqlx::Error),
}
