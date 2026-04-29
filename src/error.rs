use thiserror::Error;

use crate::config::SettingsError;
use crate::provider::ProviderError;
use crate::types::ParseError;

/// Cross-cutting application errors raised during startup / composition. Library APIs
/// have their own error types per CLAUDE.md §12 — this is the binary-edge type that
/// `main` propagates through `anyhow`.
#[derive(Debug, Error)]
pub enum AppError {
    #[error("configuration: {0}")]
    Config(#[from] SettingsError),

    #[error("invalid value: {0}")]
    Parse(#[from] ParseError),

    #[error("provider init: {0}")]
    Provider(#[from] ProviderError),

    #[error("http client init: {0}")]
    Http(#[from] reqwest::Error),
}
