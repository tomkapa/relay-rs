use sqlx::PgPool;

use crate::agents::SharedAgentStore;
use crate::mcp::{McpRefreshTrigger, SharedMcpServerStore};
use crate::memory::SharedMemoryStore;
use crate::runtime::{
    SharedDagBudget, SharedLeaseManager, SharedPromptQueue, SharedResponseSource,
    SharedThreadStream,
};
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
    pub agents: SharedAgentStore,
    /// DAG turn-budget handle. Threaded through state so `send_message`
    /// can `bump_or_fail` and the worker's quiescence trigger can query
    /// liveness without re-constructing the impl.
    pub dag: SharedDagBudget,
    /// Operator-side memory access (doc/memory.md §1.9). HTTP routes
    /// under `/agents/{id}/memory*` read and mutate through this handle.
    pub memory_store: SharedMemoryStore,
    pub mcp_store: SharedMcpServerStore,
    /// Send-half of the MCP refresh signal. Cheap to clone; CRUD handlers fire it
    /// after every write. The owning coordinator task lives on [`Server`].
    pub mcp_refresh: McpRefreshTrigger,
    /// Fan-in DAG stream — `GET /threads/{id}/stream` subscribes here. The
    /// owning task is held by [`Server`]; this handle is cheap to clone.
    pub thread_stream: SharedThreadStream,
    /// Shared connection pool for threads-route SQL (channel feed + thread
    /// history). The trait surface for those queries is small enough to keep
    /// inline in the route module rather than spinning up another store
    /// abstraction; this field is the seam.
    pub pool: PgPool,
}
