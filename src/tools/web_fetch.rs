use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use reqwest::Client;
use reqwest::redirect::Policy;
use serde::Deserialize;
use serde_json::{Value, json};
use tracing::instrument;

use crate::types::ToolName;

use super::limits::{FETCH_MAX_BODY_BYTES, FETCH_MAX_REDIRECTS, FETCH_TIMEOUT};
use super::traits::{Tool, ToolError};
use super::url::{FetchUrl, UrlError, check_host};

const FETCH_USER_AGENT: &str = concat!("relay-rs/", env!("CARGO_PKG_VERSION"));
const FETCH_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Fetch the body of a single HTTPS URL.
///
/// SSRF defence is layered: [`FetchUrl`] rejects bad URLs at parse time, and the redirect
/// policy re-checks the destination on every hop so a public URL cannot redirect into an
/// internal target.
#[derive(Debug)]
pub struct WebFetchTool {
    name: ToolName,
    schema: Arc<Value>,
    client: Client,
}

impl WebFetchTool {
    pub fn new() -> Result<Self, reqwest::Error> {
        Ok(Self {
            name: ToolName::try_from("web_fetch").expect("static name is valid"),
            schema: Arc::new(json!({
                "type": "object",
                "properties": {
                    "url": {
                        "type": "string",
                        "description": "Fully-qualified https:// URL to fetch."
                    }
                },
                "required": ["url"],
                "additionalProperties": false
            })),
            client: build_client()?,
        })
    }
}

/// Build a fetch-specific HTTP client. Reqwest doesn't let us mutate redirect policy on
/// an existing client, so the agent's general client and the fetch client are siblings.
/// Cost is one extra connection pool — well worth the SSRF guarantee.
fn build_client() -> Result<Client, reqwest::Error> {
    Client::builder()
        .timeout(FETCH_TIMEOUT)
        .connect_timeout(FETCH_CONNECT_TIMEOUT)
        .user_agent(FETCH_USER_AGENT)
        .https_only(true)
        .redirect(Policy::custom(|attempt| {
            if attempt.previous().len() >= FETCH_MAX_REDIRECTS {
                return attempt.error("too many redirects");
            }
            match attempt.url().host() {
                Some(host) => match check_host(&host) {
                    Ok(()) => attempt.follow(),
                    Err(e) => attempt.error(e),
                },
                None => attempt.error(UrlError::HostMissing),
            }
        }))
        .build()
}

#[derive(Debug, Deserialize)]
struct Input {
    url: String,
}

#[async_trait]
impl Tool for WebFetchTool {
    fn name(&self) -> &ToolName {
        &self.name
    }

    fn description(&self) -> &str {
        "Fetch the contents of a single https:// URL and return the response body \
         (truncated to 200 KB). Use this to read documentation, articles, or any \
         text/HTML page the user references. URLs to private / loopback / metadata \
         hosts are refused."
    }

    fn input_schema(&self) -> Arc<Value> {
        self.schema.clone()
    }

    #[instrument(name = "tool.web_fetch", skip_all, fields(relay.tool = "web_fetch"))]
    async fn execute(&self, input: Value) -> Result<String, ToolError> {
        let Input { url } = serde_json::from_value(input)
            .map_err(|e| ToolError::InvalidInput(format!("web_fetch: {e}")))?;
        let url = FetchUrl::try_from(url.as_str())?;

        let response = self.client.get(url.as_str()).send().await?;
        let status = response.status();
        let bytes = response.bytes().await?;

        // §6: response.bytes() never returns more than the connection saw, but we still
        // assert the slice arithmetic — cheap and defends against API drift.
        let take = bytes.len().min(FETCH_MAX_BODY_BYTES);
        assert!(take <= bytes.len());
        let body = String::from_utf8_lossy(&bytes[..take]).into_owned();

        if !status.is_success() {
            return Err(ToolError::Upstream {
                status: status.as_u16(),
                body,
            });
        }

        if bytes.len() > FETCH_MAX_BODY_BYTES {
            Ok(format!(
                "{body}\n\n[truncated to {FETCH_MAX_BODY_BYTES} bytes of {} total]",
                bytes.len()
            ))
        } else {
            Ok(body)
        }
    }
}
