use thiserror::Error;

use crate::types::ParseError;

use super::types::AgentId;

/// All failure modes of the agents subsystem. CLAUDE.md §12: one error per module
/// boundary so callers exhaustively match.
#[derive(Debug, Error)]
pub enum AgentStoreError {
    #[error("agent {0:?} not found")]
    NotFound(AgentId),

    /// `default_id()` was called before the init seeder ran. A correctly composed
    /// `Server` always seeds before exposing the store, so this is a programmer
    /// error in test setup or a misordered startup, not a runtime condition.
    #[error("no default agent has been seeded")]
    NoDefault,

    #[error("agent record decode: {0}")]
    Parse(#[from] ParseError),

    #[error("agent store backend error: {0}")]
    Backend(String),

    #[error("agent store db error: {0}")]
    Db(#[from] sqlx::Error),
}
