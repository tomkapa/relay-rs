use thiserror::Error;

#[derive(Debug, Error)]
pub enum AgentError {
    #[error("anthropic api error: {0}")]
    Api(#[from] claudius::Error),

    #[error("max turns ({0}) exceeded without final reply")]
    MaxTurnsExceeded(u32),

    #[error("model returned empty reply")]
    EmptyReply,
}
