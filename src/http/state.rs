use crate::mcp::{McpRefreshTrigger, SharedMcpServerStore};
use crate::runtime::{SharedLeaseManager, SharedPromptQueue, SharedResponseSource};
use crate::session::SharedSessionStore;

/// Cheaply-cloneable container of every collaborator the HTTP routes need. The router
/// gets a single `AppState` and threads it through axum's extractors.
#[derive(Clone, Debug)]
pub struct AppState {
    pub queue: SharedPromptQueue,
    #[allow(dead_code)] // surfaced for future endpoints (lease admin) and Postgres parity.
    pub leases: SharedLeaseManager,
    pub responses: SharedResponseSource,
    pub sessions: SharedSessionStore,
    pub mcp_store: SharedMcpServerStore,
    /// Send-half of the MCP refresh signal. Cheap to clone; CRUD handlers fire it
    /// after every write. The owning coordinator task lives on [`Server`].
    pub mcp_refresh: McpRefreshTrigger,
}
