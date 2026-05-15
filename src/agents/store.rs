//! Storage trait + cheap-clone handle for the agents registry.

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;

use super::error::AgentStoreError;
use super::types::{
    AgentCard, AgentDescription, AgentId, AgentName, AgentRecord, AgentSystemPrompt,
    AllowedMcpServers,
};

/// Input to [`AgentStore::create`]. Server-side fields (`id`, `created_at`,
/// `updated_at`) are minted by the store; never carried in.
#[derive(Debug, Clone)]
pub struct NewAgent {
    pub name: AgentName,
    pub system_prompt: AgentSystemPrompt,
    /// Operator-curated, model-facing one-sentence blurb. Required at
    /// create time — there is no "empty description" path.
    pub description: AgentDescription,
    /// When `true`, the new row becomes the default; the previously-default row
    /// is demoted in the same transaction so the partial unique index is
    /// satisfied.
    pub is_default: bool,
    /// Initial MCP allowlist. Empty for the no-MCP-tools default; the operator
    /// supplies it explicitly when granting access at create time.
    pub allowed_mcp_servers: AllowedMcpServers,
}

/// HTTP-PATCH-style update payload.
///
/// Each field's outer `Option` distinguishes "field omitted (no change)" from
/// "field present (set)". `is_default = Some(true)` triggers an atomic
/// demote-then-promote; `is_default = Some(false)` is rejected when applied to
/// the current default row (the system requires a default to exist at all
/// times). `allowed_mcp_servers = Some(<empty>)` is the lockdown path —
/// distinct from "field omitted" so HTTP PATCH can revoke every server.
#[derive(Debug, Clone, Default)]
pub struct AgentUpdate {
    pub name: Option<AgentName>,
    pub system_prompt: Option<AgentSystemPrompt>,
    /// Patch the description. Required, non-empty if present — the
    /// newtype's `TryFrom` rejects empty/whitespace at the HTTP boundary.
    pub description: Option<AgentDescription>,
    pub is_default: Option<bool>,
    pub allowed_mcp_servers: Option<AllowedMcpServers>,
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

    /// Case-insensitive lookup by [`AgentName`]. Returns the matching
    /// record on success; [`AgentStoreError::NameNotFound`] when no row
    /// matches. Powers the model-facing addressing surfaces — the
    /// `send_message` tool resolves `{kind:"agent", name:<role>}` through
    /// here.
    async fn read_by_name(&self, name: &AgentName) -> Result<AgentRecord, AgentStoreError>;

    /// Snapshot of every row's `(id, name)` pair, ordered alphabetically
    /// by `lower(name)`. Used to render the `<agents>` block; the renderer
    /// excludes the caller before formatting. Distinct from [`Self::list`]
    /// so the caller can skip hydrating columns it does not need.
    async fn list_names(&self) -> Result<Vec<(AgentId, AgentName)>, AgentStoreError>;

    /// Top-K cosine-similarity search over agents' description embeddings.
    /// Returns slim cards sorted by descending similarity, capped at `k`,
    /// with `viewer` excluded at the SQL boundary so the caller never
    /// pays decode cost for the self row. Rows whose embedding is null
    /// (not yet backfilled) are skipped — the caller treats an empty
    /// result as a degraded layer rather than an error.
    async fn search_by_description(
        &self,
        embedding: &[f32],
        viewer: AgentId,
        k: usize,
    ) -> Result<Vec<AgentCard>, AgentStoreError>;
}

/// Cheap-clone handle so collaborators can hold the store without a generic
/// parameter.
pub type SharedAgentStore = Arc<dyn AgentStore>;
