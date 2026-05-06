//! Storage trait + cheap-clone handle for the agents registry.
//!
//! Today the trait surface is intentionally narrow: per-turn `read(id)` and
//! session-create `default_id()`. CRUD endpoints will land later as additional
//! methods; the worker hot path needs only these two.

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;

use super::error::AgentStoreError;
use super::types::{AgentId, AgentRecord};

/// Storage trait for the agents registry. Implementations must be thread-safe.
#[async_trait]
pub trait AgentStore: fmt::Debug + Send + Sync {
    /// Fetch a single agent by id. Returns
    /// [`AgentStoreError::NotFound`] when the id is unknown — the caller decides
    /// whether to map that to a 404 (operator lookup) or a 400 (session-create
    /// pointed at a deleted agent).
    async fn read(&self, id: AgentId) -> Result<AgentRecord, AgentStoreError>;

    /// Id of the row whose `is_default` flag is `TRUE`. Returns
    /// [`AgentStoreError::NoDefault`] when the seeder has not run yet — the
    /// composition root must call [`super::seed_default`] before exposing the
    /// store.
    async fn default_id(&self) -> Result<AgentId, AgentStoreError>;
}

/// Cheap-clone handle so collaborators can hold the store without a generic
/// parameter.
pub type SharedAgentStore = Arc<dyn AgentStore>;
