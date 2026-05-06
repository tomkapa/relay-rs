//! Thin wrapper around `rmcp` that hides the SDK's transport, handler and `Peer`
//! plumbing behind a small surface (`connect`, `list_tools`, `call_tool`).
//!
//! Holds the running service so the worker task and HTTP connection stay alive for as
//! long as `McpClient` does. Dropping the client cancels the worker (rmcp signals on
//! drop), which closes the HTTP/SSE connection cleanly.

use std::collections::HashMap;
use std::sync::Arc;

use http::header::{HeaderName, HeaderValue};
use rmcp::ServiceExt;
use rmcp::model::{
    CallToolRequestParams, CallToolResult, JsonObject, PaginatedRequestParams, Tool,
};
use rmcp::service::{RoleClient, RunningService};
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use serde_json::Value;
use tokio::time::timeout;

use super::error::McpError;
use super::limits::{MCP_CALL_TIMEOUT, MCP_CONNECT_TIMEOUT, MCP_LIST_TOOLS_TIMEOUT};
use super::types::McpTransport;

/// Connected MCP server. Cheap to clone (the running service lives behind an `Arc`)
/// so multiple `McpTool` instances can share one upstream connection.
pub struct McpClient {
    inner: Arc<RunningService<RoleClient, ()>>,
}

impl std::fmt::Debug for McpClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpClient").finish_non_exhaustive()
    }
}

impl McpClient {
    /// Open a connection to `transport` and complete the MCP `initialize` handshake
    /// under [`MCP_CONNECT_TIMEOUT`].
    pub async fn connect(transport: &McpTransport) -> Result<Self, McpError> {
        match transport {
            McpTransport::Http { url, headers } => {
                let mut custom_headers: HashMap<HeaderName, HeaderValue> =
                    HashMap::with_capacity(headers.len());
                for (name, value) in headers {
                    let header_name = HeaderName::try_from(name.as_str()).map_err(|e| {
                        McpError::InvalidConfig(format!("header name `{}`: {e}", name.as_str()))
                    })?;
                    let header_value = HeaderValue::from_str(value.as_str()).map_err(|e| {
                        McpError::InvalidConfig(format!("header value rejected: {e}"))
                    })?;
                    custom_headers.insert(header_name, header_value);
                }
                let cfg = StreamableHttpClientTransportConfig::with_uri(url.as_str())
                    .custom_headers(custom_headers);
                let transport = StreamableHttpClientTransport::from_config(cfg);
                let connect = timeout(MCP_CONNECT_TIMEOUT, ().serve(transport))
                    .await
                    .map_err(|_| McpError::Connect("initialize timed out".into()))?
                    .map_err(|e| McpError::Connect(e.to_string()))?;
                Ok(Self {
                    inner: Arc::new(connect),
                })
            }
        }
    }

    /// Fetch the full tool list from the server, paginating until `next_cursor` is
    /// `None`. Bounded by [`MCP_LIST_TOOLS_TIMEOUT`] across all pages.
    pub async fn list_tools(&self) -> Result<Vec<Tool>, McpError> {
        let peer = self.inner.peer().clone();
        let fut = async move {
            let mut tools = Vec::new();
            let mut cursor = None;
            loop {
                let result = peer
                    .list_tools(Some(PaginatedRequestParams::default().with_cursor(cursor)))
                    .await
                    .map_err(|e| McpError::ListTools(e.to_string()))?;
                tools.extend(result.tools);
                cursor = result.next_cursor;
                if cursor.is_none() {
                    break;
                }
            }
            Ok::<_, McpError>(tools)
        };
        timeout(MCP_LIST_TOOLS_TIMEOUT, fut)
            .await
            .map_err(|_| McpError::ListTools("timed out".into()))?
    }

    /// Invoke `tools/call`, capped by [`MCP_CALL_TIMEOUT`].
    pub async fn call_tool(&self, name: &str, input: Value) -> Result<CallToolResult, McpError> {
        let arguments = match input {
            Value::Null => None,
            Value::Object(map) => Some(map),
            other => Some(json_object_with_value("input", other)),
        };
        let mut params = CallToolRequestParams::new(name.to_owned());
        if let Some(args) = arguments {
            params = params.with_arguments(args);
        }
        let peer = self.inner.peer().clone();
        let call = async move {
            peer.call_tool(params)
                .await
                .map_err(|e| McpError::Call(e.to_string()))
        };
        timeout(MCP_CALL_TIMEOUT, call)
            .await
            .unwrap_or_else(|_elapsed| Err(McpError::CallTimeout))
    }
}

fn json_object_with_value(field: &str, value: Value) -> JsonObject {
    let mut map = serde_json::Map::with_capacity(1);
    map.insert(field.to_owned(), value);
    map
}
