use std::fmt::Write;
use std::sync::Arc;

use async_trait::async_trait;
use reqwest::Client;
use serde::Deserialize;
use serde_json::{Value, json};
use tracing::instrument;

use crate::types::{SecretString, ToolName};

use super::limits::{SEARCH_DEFAULT_COUNT, SEARCH_MAX_COUNT, SEARCH_TIMEOUT};
use super::traits::{Tool, ToolError};

const BRAVE_ENDPOINT: &str = "https://api.search.brave.com/res/v1/web/search";
const SEARCH_QUERY_MAX_BYTES: usize = 400;

/// Brave Search front-end. Behind the trait so the agent never knows which search
/// vendor is in use; swapping vendors is one new file plus one composition-root edit.
#[derive(Debug)]
pub struct WebSearchTool {
    name: ToolName,
    schema: Arc<Value>,
    client: Client,
    api_key: SecretString,
}

impl WebSearchTool {
    pub fn new(client: Client, api_key: SecretString) -> Self {
        Self {
            name: ToolName::try_from("web_search").expect("static name is valid"),
            schema: Arc::new(json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Search query.",
                        "maxLength": SEARCH_QUERY_MAX_BYTES,
                    },
                    "count": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": SEARCH_MAX_COUNT,
                        "description": "Number of results to return (1-10, default 5)."
                    }
                },
                "required": ["query"],
                "additionalProperties": false
            })),
            client,
            api_key,
        }
    }
}

#[derive(Debug, Deserialize)]
struct Input {
    query: String,
    #[serde(default)]
    count: Option<u8>,
}

#[derive(Debug, Deserialize)]
struct BraveResponse {
    #[serde(default)]
    web: Option<BraveWeb>,
}

#[derive(Debug, Deserialize)]
struct BraveWeb {
    #[serde(default)]
    results: Vec<BraveResult>,
}

#[derive(Debug, Deserialize)]
struct BraveResult {
    #[serde(default)]
    title: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    description: String,
}

#[async_trait]
impl Tool for WebSearchTool {
    fn name(&self) -> &ToolName {
        &self.name
    }

    fn description(&self) -> &str {
        "Search the public web via the Brave Search API and return the top results \
         (title, url, snippet). Use when the user asks about something requiring \
         up-to-date information."
    }

    fn input_schema(&self) -> Arc<Value> {
        self.schema.clone()
    }

    #[instrument(name = "tool.web_search", skip_all, fields(relay.tool = "web_search"))]
    async fn execute(&self, input: Value) -> Result<String, ToolError> {
        let Input { query, count } = serde_json::from_value(input)
            .map_err(|e| ToolError::InvalidInput(format!("web_search: {e}")))?;

        let trimmed = query.trim();
        if trimmed.is_empty() {
            return Err(ToolError::InvalidInput("query must not be empty".into()));
        }
        if trimmed.len() > SEARCH_QUERY_MAX_BYTES {
            return Err(ToolError::InvalidInput(format!(
                "query exceeds {SEARCH_QUERY_MAX_BYTES} bytes"
            )));
        }

        let count = count
            .unwrap_or(SEARCH_DEFAULT_COUNT)
            .clamp(1, SEARCH_MAX_COUNT);

        let response = self
            .client
            .get(BRAVE_ENDPOINT)
            .timeout(SEARCH_TIMEOUT)
            .header("Accept", "application/json")
            .header("X-Subscription-Token", self.api_key.expose())
            .query(&[("q", trimmed), ("count", count.to_string().as_str())])
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(ToolError::Upstream {
                status: status.as_u16(),
                body,
            });
        }

        let parsed: BraveResponse = response.json().await?;
        let results = parsed.web.map(|w| w.results).unwrap_or_default();

        if results.is_empty() {
            return Ok(format!("No results for query: {trimmed}"));
        }

        let mut out = String::with_capacity(results.len() * 256);
        for (i, r) in results.iter().enumerate() {
            // §6: writing into a String is infallible — using `write!` keeps clippy happy
            // about avoiding repeated `format!` allocations and asserts that fact.
            let res = writeln!(
                out,
                "{idx}. {title}\n   {url}\n   {desc}\n",
                idx = i + 1,
                title = r.title,
                url = r.url,
                desc = r.description,
            );
            assert!(res.is_ok(), "writing to String never errors");
        }
        Ok(out)
    }
}
