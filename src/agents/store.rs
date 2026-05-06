//! Storage trait + cheap-clone handle for the agents registry.

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;

use super::error::AgentStoreError;
use super::types::{AgentId, AgentName, AgentRecord, AgentSystemPrompt};

/// Input to [`AgentStore::create`]. Server-side fields (`id`, `created_at`,
/// `updated_at`) are minted by the store; never carried in.
#[derive(Debug, Clone)]
pub struct NewAgent {
    pub name: AgentName,
    pub system_prompt: AgentSystemPrompt,
    /// When `true`, the new row becomes the default; the previously-default row
    /// is demoted in the same transaction so the partial unique index is
    /// satisfied.
    pub is_default: bool,
}

/// HTTP-PATCH-style update payload.
///
/// Each field's outer `Option` distinguishes "field omitted (no change)" from
/// "field present (set)". `is_default = Some(true)` triggers an atomic
/// demote-then-promote; `is_default = Some(false)` is rejected when applied to
/// the current default row (the system requires a default to exist at all
/// times).
#[derive(Debug, Clone, Default)]
pub struct AgentUpdate {
    pub name: Option<AgentName>,
    pub system_prompt: Option<AgentSystemPrompt>,
    pub is_default: Option<bool>,
}

/// Storage trait for the agents registry. Implementations must be thread-safe.
#[async_trait]
pub trait AgentStore: fmt::Debug + Send + Sync {
    /// Mint a new agent row.
    async fn create(&self, payload: NewAgent) -> Result<AgentRecord, AgentStoreError>;

    /// Snapshot of every row, ordered by `created_at` ascending.
    async fn list(&self) -> Result<Vec<AgentRecord>, AgentStoreError>;

    /// Fetch a single agent by id.
    async fn read(&self, id: AgentId) -> Result<AgentRecord, AgentStoreError>;

    /// Patch the row with whatever subset of fields is set on `payload`.
    async fn update(
        &self,
        id: AgentId,
        payload: AgentUpdate,
    ) -> Result<AgentRecord, AgentStoreError>;

    /// Remove the row. Refuses to delete the row currently flagged as default
    /// (returns [`AgentStoreError::DefaultDeletionForbidden`]).
    async fn delete(&self, id: AgentId) -> Result<(), AgentStoreError>;

    /// Id of the row whose `is_default` flag is `TRUE`.
    async fn default_id(&self) -> Result<AgentId, AgentStoreError>;
}

/// Cheap-clone handle so collaborators can hold the store without a generic
/// parameter.
pub type SharedAgentStore = Arc<dyn AgentStore>;
