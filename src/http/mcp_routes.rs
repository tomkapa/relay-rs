//! CRUD endpoints for the MCP server registry.
//!
//! `POST /mcp-servers`        — create
//! `GET  /mcp-servers`        — list
//! `GET  /mcp-servers/{id}`   — read one
//! `PUT  /mcp-servers/{id}`   — update
//! `DELETE /mcp-servers/{id}` — delete
//!
//! Every mutating handler signals the long-running MCP refresh coordinator (via the
//! cheap clone-able [`McpRefreshTrigger`]) so the registered tools become callable on
//! the next prompt without a restart. The CRUD response itself does not wait on the
//! refresh — operator visibility comes from the `last_seen_at`/`last_error`/
//! `discovered_tools` columns surfaced on the next read.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::mcp::{
    DiscoveredTool, McpDescription, McpServerAlias, McpServerCreate, McpServerId, McpServerRecord,
    McpServerUpdate, McpTransport,
};

use super::error::HttpError;
use super::state::AppState;

/// What we hand back on every CRUD response. Mirrors `mcp_servers` plus a flag
/// telling the operator whether the row is currently exposed by the live registry.
#[derive(Debug, Serialize)]
pub(super) struct McpServerResponse {
    pub(super) id: McpServerId,
    pub(super) alias: String,
    pub(super) enabled: bool,
    pub(super) config: McpTransport,
    pub(super) description: Option<String>,
    pub(super) last_seen_at: Option<chrono::DateTime<chrono::Utc>>,
    pub(super) last_error: Option<String>,
    pub(super) discovered_tools: Option<Vec<DiscoveredTool>>,
    pub(super) created_at: chrono::DateTime<chrono::Utc>,
    pub(super) updated_at: chrono::DateTime<chrono::Utc>,
}

impl From<McpServerRecord> for McpServerResponse {
    fn from(r: McpServerRecord) -> Self {
        Self {
            id: r.id,
            alias: r.alias.as_str().to_owned(),
            enabled: r.enabled,
            config: r.config,
            description: r.description.map(|d| d.as_str().to_owned()),
            last_seen_at: r.last_seen_at,
            last_error: r.last_error,
            discovered_tools: r.discovered_tools,
            created_at: r.created_at,
            updated_at: r.updated_at,
        }
    }
}

#[derive(Debug, Deserialize)]
pub(super) struct CreateMcpServerRequest {
    alias: String,
    config: McpTransport,
    #[serde(default)]
    description: Option<String>,
    #[serde(default = "default_enabled")]
    enabled: bool,
}

fn default_enabled() -> bool {
    true
}

#[derive(Debug, Deserialize)]
pub(super) struct UpdateMcpServerRequest {
    #[serde(default)]
    alias: Option<String>,
    #[serde(default)]
    config: Option<McpTransport>,
    /// HTTP PATCH semantics: outer `Option` distinguishes "field omitted (no change)"
    /// from "field present (set or clear)"; inner `Option` carries the new value
    /// (`null` clears the column). Clippy's suggested "custom enum" alternative is
    /// strictly more boilerplate for the same shape.
    #[serde(default, deserialize_with = "double_option::deserialize")]
    #[allow(clippy::option_option)] // explicit two-state representation; see doc comment
    description: Option<Option<String>>,
    #[serde(default)]
    enabled: Option<bool>,
}

mod double_option {
    use serde::{Deserialize, Deserializer};

    #[allow(clippy::option_option)] // see UpdateMcpServerRequest::description
    pub(super) fn deserialize<'de, D, T>(d: D) -> Result<Option<Option<T>>, D::Error>
    where
        D: Deserializer<'de>,
        T: Deserialize<'de>,
    {
        Deserialize::deserialize(d).map(Some)
    }
}

pub(super) async fn create_mcp_server(
    State(state): State<AppState>,
    Json(payload): Json<CreateMcpServerRequest>,
) -> Result<(StatusCode, Json<McpServerResponse>), HttpError> {
    let alias = McpServerAlias::try_from(payload.alias).map_err(HttpError::Parse)?;
    let description = payload
        .description
        .map(McpDescription::try_from)
        .transpose()
        .map_err(HttpError::Parse)?;
    let record = state
        .mcp_store
        .create(McpServerCreate {
            alias,
            config: payload.config,
            description,
            enabled: payload.enabled,
        })
        .await?;
    state.mcp_refresh.request();
    Ok((StatusCode::CREATED, Json(record.into())))
}

pub(super) async fn list_mcp_servers(
    State(state): State<AppState>,
) -> Result<Json<Vec<McpServerResponse>>, HttpError> {
    let rows = state.mcp_store.list().await?;
    Ok(Json(
        rows.into_iter().map(McpServerResponse::from).collect(),
    ))
}

pub(super) async fn read_mcp_server(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<McpServerResponse>, HttpError> {
    let id = McpServerId::from(id);
    let row = state.mcp_store.read(id).await?;
    Ok(Json(row.into()))
}

pub(super) async fn update_mcp_server(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(payload): Json<UpdateMcpServerRequest>,
) -> Result<Json<McpServerResponse>, HttpError> {
    let id = McpServerId::from(id);
    let alias = payload
        .alias
        .map(McpServerAlias::try_from)
        .transpose()
        .map_err(HttpError::Parse)?;
    // Outer Some carries through; inner Some maps to the typed value, inner None
    // (explicit clear) stays None — `Option::map` over the inner Option does both at
    // once without unrolling the four states by hand.
    let description = payload
        .description
        .map(|inner| inner.map(McpDescription::try_from).transpose())
        .transpose()
        .map_err(HttpError::Parse)?;
    let row = state
        .mcp_store
        .update(
            id,
            McpServerUpdate {
                alias,
                config: payload.config,
                description,
                enabled: payload.enabled,
            },
        )
        .await?;
    state.mcp_refresh.request();
    Ok(Json(row.into()))
}

pub(super) async fn delete_mcp_server(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, HttpError> {
    let id = McpServerId::from(id);
    state.mcp_store.delete(id).await?;
    state.mcp_refresh.request();
    Ok(StatusCode::NO_CONTENT)
}
