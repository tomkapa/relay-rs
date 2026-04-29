use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use thiserror::Error;

use crate::types::ToolName;

use super::url::UrlError;

#[derive(Debug, Error)]
pub enum ToolError {
    #[error("invalid input: {0}")]
    InvalidInput(String),

    #[error("disallowed url: {0}")]
    DisallowedUrl(#[from] UrlError),

    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("upstream returned status {status}: {body}")]
    Upstream { status: u16, body: String },

    #[error("tool returned a result that exceeded the size cap ({max} bytes)")]
    ResultTooLarge { max: usize },

    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

/// A side-effecting capability the model can request.
///
/// Implementations must be cheap to clone (they go behind `Arc`) and must validate every
/// input from the model — never trust `input: Value` shape, parse it through `serde` into
/// a typed struct.
#[async_trait]
pub trait Tool: Send + Sync + std::fmt::Debug {
    /// Stable, lower-case identifier. Validated at registration through [`ToolName`].
    fn name(&self) -> &ToolName;

    /// Human-readable description shown to the model. Be specific — vague descriptions
    /// produce vague tool calls.
    fn description(&self) -> &str;

    /// JSON-schema description of the tool's input. Cached on the tool struct so the
    /// agent does not re-allocate it every turn.
    fn input_schema(&self) -> Arc<Value>;

    async fn execute(&self, input: Value) -> Result<String, ToolError>;
}

/// Cheap-clone alias used by the registry.
pub type SharedTool = Arc<dyn Tool>;
