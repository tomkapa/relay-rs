use std::net::SocketAddr;

use config::{Config, ConfigError, Environment};
use serde::Deserialize;
use thiserror::Error;

use crate::types::{ModelId, SecretString};

#[derive(Debug, Error)]
pub enum SettingsError {
    #[error("config source: {0}")]
    Source(#[from] ConfigError),
}

/// Process-wide configuration loaded once at startup. Secrets are wrapped in
/// [`SecretString`] so a stray `tracing::debug!(?settings)` cannot leak them.
#[derive(Debug, Clone, Deserialize)]
pub struct Settings {
    pub anthropic_api_key: SecretString,
    #[serde(default)]
    pub anthropic_base_url: Option<String>,
    pub brave_search_api_key: SecretString,
    #[serde(default = "default_model")]
    pub model: ModelId,
    #[serde(default = "default_http_addr")]
    pub http_addr: SocketAddr,
}

fn default_model() -> ModelId {
    ModelId::try_from("claude-sonnet-4-5").expect("static default model id is valid")
}

fn default_http_addr() -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], 8080))
}

impl Settings {
    /// Load settings from environment variables. Missing required values surface as a
    /// `SettingsError::Source` so the caller can decide how to report.
    pub fn load() -> Result<Self, SettingsError> {
        let raw = Config::builder()
            .add_source(Environment::default())
            .build()?;
        Ok(raw.try_deserialize()?)
    }
}
