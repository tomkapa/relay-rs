pub mod limits;
mod registry;
mod web_fetch;
mod web_search;

use std::sync::Arc;

use async_trait::async_trait;
use claudius::{ToolParam, ToolUnionParam};
use serde_json::Value;
use thiserror::Error;

pub use registry::ToolRegistry;
pub use web_fetch::WebFetchTool;
pub use web_search::WebSearchTool;

use crate::app::AppState;

#[derive(Debug, Error)]
pub enum ToolError {
    #[error("invalid input: {0}")]
    InvalidInput(String),

    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("upstream returned status {status}: {body}")]
    Upstream { status: u16, body: String },

    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn input_schema(&self) -> Value;

    async fn execute(&self, input: Value) -> Result<String, ToolError>;
}

impl From<&dyn Tool> for ToolUnionParam {
    fn from(tool: &dyn Tool) -> Self {
        ToolUnionParam::CustomTool(
            ToolParam::new(tool.name().to_string(), tool.input_schema())
                .with_description(tool.description().to_string()),
        )
    }
}

pub fn default_registry(state: &AppState) -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(WebFetchTool::new(state.http().clone())));
    registry.register(Arc::new(WebSearchTool::new(
        state.http().clone(),
        state.settings().brave_search_api_key.clone(),
    )));
    registry
}
