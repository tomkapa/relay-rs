use async_trait::async_trait;
use reqwest::Client;
use serde::Deserialize;
use serde_json::{Value, json};

use super::limits::{FETCH_MAX_BODY_BYTES, FETCH_TIMEOUT};
use super::{Tool, ToolError};

pub struct WebFetchTool {
    client: Client,
}

impl WebFetchTool {
    pub fn new(client: Client) -> Self {
        Self { client }
    }
}

#[derive(Debug, Deserialize)]
struct Input {
    url: String,
}

#[async_trait]
impl Tool for WebFetchTool {
    fn name(&self) -> &str {
        "web_fetch"
    }

    fn description(&self) -> &str {
        "Fetch the contents of a single URL over HTTPS and return the raw response body \
         (truncated to 200KB). Use this to read documentation, articles, or any text/HTML page \
         the user references. Pass a fully-qualified URL."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "Fully-qualified http(s) URL to fetch."
                }
            },
            "required": ["url"]
        })
    }

    async fn execute(&self, input: Value) -> Result<String, ToolError> {
        let Input { url } = serde_json::from_value(input)
            .map_err(|e| ToolError::InvalidInput(format!("web_fetch: {e}")))?;

        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return Err(ToolError::InvalidInput(
                "url must start with http:// or https://".into(),
            ));
        }

        let response = self
            .client
            .get(&url)
            .timeout(FETCH_TIMEOUT)
            .send()
            .await?;
        let status = response.status();
        let bytes = response.bytes().await?;

        let truncated = bytes.len() > FETCH_MAX_BODY_BYTES;
        let slice = &bytes[..bytes.len().min(FETCH_MAX_BODY_BYTES)];
        let body = String::from_utf8_lossy(slice).into_owned();

        if !status.is_success() {
            return Err(ToolError::Upstream {
                status: status.as_u16(),
                body,
            });
        }

        if truncated {
            Ok(format!(
                "{body}\n\n[truncated to {FETCH_MAX_BODY_BYTES} bytes of {} total]",
                bytes.len()
            ))
        } else {
            Ok(body)
        }
    }
}
