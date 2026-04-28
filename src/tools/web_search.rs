use async_trait::async_trait;
use reqwest::Client;
use serde::Deserialize;
use serde_json::{Value, json};

use super::limits::{SEARCH_DEFAULT_COUNT, SEARCH_MAX_COUNT, SEARCH_TIMEOUT};
use super::{Tool, ToolError};

const BRAVE_ENDPOINT: &str = "https://api.search.brave.com/res/v1/web/search";

pub struct WebSearchTool {
    client: Client,
    api_key: String,
}

impl WebSearchTool {
    pub fn new(client: Client, api_key: String) -> Self {
        Self { client, api_key }
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
    fn name(&self) -> &str {
        "web_search"
    }

    fn description(&self) -> &str {
        "Search the public web via the Brave Search API and return the top results \
         (title, url, snippet). Use this when the user asks about something you may not \
         know up-to-date information about, or when they ask you to look something up."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "The search query."
                },
                "count": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": SEARCH_MAX_COUNT,
                    "description": "Number of results to return (1-10, default 5)."
                }
            },
            "required": ["query"]
        })
    }

    async fn execute(&self, input: Value) -> Result<String, ToolError> {
        let Input { query, count } = serde_json::from_value(input)
            .map_err(|e| ToolError::InvalidInput(format!("web_search: {e}")))?;

        if query.trim().is_empty() {
            return Err(ToolError::InvalidInput("query must not be empty".into()));
        }

        let count = count
            .unwrap_or(SEARCH_DEFAULT_COUNT)
            .clamp(1, SEARCH_MAX_COUNT);

        let response = self
            .client
            .get(BRAVE_ENDPOINT)
            .timeout(SEARCH_TIMEOUT)
            .header("Accept", "application/json")
            .header("X-Subscription-Token", &self.api_key)
            .query(&[("q", query.as_str()), ("count", count.to_string().as_str())])
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
            return Ok(format!("No results for query: {query}"));
        }

        let mut out = String::with_capacity(results.len() * 256);
        for (i, r) in results.iter().enumerate() {
            out.push_str(&format!(
                "{idx}. {title}\n   {url}\n   {desc}\n\n",
                idx = i + 1,
                title = r.title,
                url = r.url,
                desc = r.description,
            ));
        }
        Ok(out)
    }
}
