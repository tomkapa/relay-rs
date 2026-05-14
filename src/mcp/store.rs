//! Storage trait for the MCP-server registry.
//!
//! The HTTP CRUD handlers and the dynamic [`McpRegistry`](super::registry::McpRegistry)
//! both go through this trait — production deployments wire in [`PgMcpServerStore`],
//! tests can wire a fake without touching Postgres.

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;

use super::error::McpError;
use super::types::{
    DiscoveredTool, McpDescription, McpServerAlias, McpServerId, McpServerRecord, McpTransport,
};

/// Boundary type that captures a CRUD-create request after validation. Lives here (not
/// in HTTP) because the registry's own bootstrapping reaches the same trait method.
#[derive(Debug, Clone)]
pub struct McpServerCreate {
    pub alias: McpServerAlias,
    pub config: McpTransport,
    pub description: Option<McpDescription>,
    pub enabled: bool,
}

/// Update payload. `None` fields keep the current value; `Some` replaces it.
#[derive(Debug, Clone, Default)]
pub struct McpServerUpdate {
    pub alias: Option<McpServerAlias>,
    pub config: Option<McpTransport>,
    pub description: Option<Option<McpDescription>>,
    pub enabled: Option<bool>,
}

/// Health update emitted by the registry after a refresh. Stored verbatim.
#[derive(Debug, Clone)]
pub struct McpHealthUpdate {
    pub last_seen_at: Option<chrono::DateTime<chrono::Utc>>,
    pub last_error: Option<String>,
    pub discovered_tools: Option<Vec<DiscoveredTool>>,
}

#[async_trait]
pub trait McpServerStore: fmt::Debug + Send + Sync {
    async fn create(&self, payload: McpServerCreate) -> Result<McpServerRecord, McpError>;
    async fn list(&self) -> Result<Vec<McpServerRecord>, McpError>;
    async fn list_enabled(&self) -> Result<Vec<McpServerRecord>, McpError>;
    async fn read(&self, id: McpServerId) -> Result<McpServerRecord, McpError>;
    async fn update(
        &self,
        id: McpServerId,
        payload: McpServerUpdate,
    ) -> Result<McpServerRecord, McpError>;
    async fn delete(&self, id: McpServerId) -> Result<(), McpError>;
    async fn update_health(&self, id: McpServerId, health: McpHealthUpdate)
    -> Result<(), McpError>;
}

pub type SharedMcpServerStore = Arc<dyn McpServerStore>;

// `pub(crate)` is the accurate visibility — the module is reachable from any
// `#[cfg(test)]` site in the crate — and `unreachable_pub` would otherwise
// trip if we used a plain `pub` on items inside a private module.
#[cfg(test)]
#[allow(clippy::redundant_pub_crate)]
pub(crate) mod test_support {
    //! Null-object [`McpServerStore`] used only by the `McpRegistry::for_test`
    //! constructor in this crate's unit tests. Every method panics — calling
    //! it would mean a test path that should have been allocation-free went
    //! through `refresh`, which is the bug.
    use super::*;
    use crate::mcp::types::{McpServerId, McpServerRecord};

    #[derive(Debug)]
    struct NullStore;

    #[async_trait]
    impl McpServerStore for NullStore {
        async fn create(&self, _: McpServerCreate) -> Result<McpServerRecord, McpError> {
            panic!("invariant: McpRegistry::for_test must not consult the store");
        }
        async fn list(&self) -> Result<Vec<McpServerRecord>, McpError> {
            panic!("invariant: McpRegistry::for_test must not consult the store");
        }
        async fn list_enabled(&self) -> Result<Vec<McpServerRecord>, McpError> {
            panic!("invariant: McpRegistry::for_test must not consult the store");
        }
        async fn read(&self, _: McpServerId) -> Result<McpServerRecord, McpError> {
            panic!("invariant: McpRegistry::for_test must not consult the store");
        }
        async fn update(
            &self,
            _: McpServerId,
            _: McpServerUpdate,
        ) -> Result<McpServerRecord, McpError> {
            panic!("invariant: McpRegistry::for_test must not consult the store");
        }
        async fn delete(&self, _: McpServerId) -> Result<(), McpError> {
            panic!("invariant: McpRegistry::for_test must not consult the store");
        }
        async fn update_health(&self, _: McpServerId, _: McpHealthUpdate) -> Result<(), McpError> {
            panic!("invariant: McpRegistry::for_test must not consult the store");
        }
    }

    pub(crate) fn null_store() -> SharedMcpServerStore {
        Arc::new(NullStore)
    }
}
