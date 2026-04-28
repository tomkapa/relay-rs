use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("config: {0}")]
    Config(#[from] config::ConfigError),

    #[error("anthropic client init: {0}")]
    Anthropic(#[from] claudius::Error),

    #[error("http client init: {0}")]
    Http(#[from] reqwest::Error),
}
