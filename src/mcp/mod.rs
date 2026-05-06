//! MCP integration: persisted server registry + dynamic tool catalogue.
//!
//! Operators register MCP servers via the HTTP API (`/mcp-servers`); rows live in
//! Postgres. [`McpRegistry`] reads the enabled rows, opens connections through
//! [`McpClient`] (rmcp wrapper), and exposes the union of remote tools as
//! [`McpTool`] instances. The agent sees them through [`crate::tools::ToolBox`].

mod client;
mod error;
mod limits;
mod pg_store;
mod refresher;
mod registry;
mod store;
mod tool;
mod types;

pub use client::McpClient;
pub use error::McpError;
pub use limits::{
    MAX_MCP_SERVERS, MAX_TOOLS_PER_SERVER, MCP_ALIAS_MAX_LEN, MCP_CALL_TIMEOUT,
    MCP_CONNECT_TIMEOUT, MCP_DESCRIPTION_MAX_LEN, MCP_HEADER_NAME_MAX_LEN,
    MCP_HEADER_VALUE_MAX_LEN, MCP_LIST_TOOLS_TIMEOUT, MCP_MAX_HEADERS, MCP_RESULT_RENDER_CAP,
    MCP_URL_MAX_LEN,
};
pub use pg_store::PgMcpServerStore;
pub use refresher::{McpRefreshTrigger, McpRefresher};
pub use registry::McpRegistry;
pub use store::{
    McpHealthUpdate, McpServerCreate, McpServerStore, McpServerUpdate, SharedMcpServerStore,
};
pub use tool::McpTool;
pub use types::{
    DiscoveredTool, McpDescription, McpHeaderName, McpHeaderValue, McpHttpUrl, McpServerAlias,
    McpServerId, McpServerRecord, McpTransport,
};
