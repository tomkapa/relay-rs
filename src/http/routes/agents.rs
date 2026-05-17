//! CRUD endpoints for the agents registry.
//!
//! `POST   /agents`        — create
//! `GET    /agents`        — list
//! `GET    /agents/{id}`   — read one
//! `PUT    /agents/{id}`   — update
//! `DELETE /agents/{id}`   — delete (refuses the default; refuses any agent
//!                          referenced by an existing session)
//!
//! Caching: an update to `system_prompt` becomes visible to live workers within
//! [`crate::agents::AGENT_PROMPT_CACHE_TTL`] (60 s) — there is no synchronous
//! invalidation of the worker's prompt cache here, by design (see the design
//! conversation: "Live prompt + 60 s TTL, no LISTEN/NOTIFY").

use axum::Json;
use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, post, put};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::agents::{
    AgentDescription, AgentId, AgentName, AgentRecord, AgentSystemPrompt, AgentUpdate,
    AllowedMcpServers, NewAgent,
};
use crate::auth::{AuthError, Principal};
use crate::mcp::McpServerId;

use super::super::error::HttpError;
use super::super::state::AppState;

pub(super) fn router() -> Router<AppState> {
    Router::new()
        .route("/agents", post(create_agent).get(list_agents))
        .route(
            "/agents/{id}",
            get(read_agent)
                .merge(put(update_agent))
                .merge(delete(delete_agent)),
        )
}

/// Wire shape returned on every agents endpoint. Mirrors the row plus
/// derived/server-managed fields.
#[derive(Debug, Serialize)]
struct AgentResponse {
    id: AgentId,
    name: String,
    system_prompt: String,
    /// Operator-curated, model-facing one-sentence blurb embedded for
    /// `search_agents`. Always present — the column is `NOT NULL`.
    description: String,
    is_default: bool,
    /// MCP server ids this agent is allowed to use tools from. Always
    /// present; an empty array means the agent has no MCP access (the
    /// default for newly minted agents).
    allowed_mcp_servers: Vec<McpServerId>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl From<AgentRecord> for AgentResponse {
    fn from(r: AgentRecord) -> Self {
        Self {
            id: r.id,
            name: r.name.as_str().to_owned(),
            system_prompt: r.system_prompt.as_str().to_owned(),
            description: r.description.as_str().to_owned(),
            is_default: r.is_default,
            allowed_mcp_servers: r.allowed_mcp_servers.into_inner(),
            created_at: r.created_at,
            updated_at: r.updated_at,
        }
    }
}

#[derive(Debug, Deserialize)]
struct CreateAgentRequest {
    name: String,
    system_prompt: String,
    /// Required, non-empty (doc/agent_discovery_plan.md §5.2). Embedded
    /// for `search_agents`.
    description: String,
    /// When `true`, the new agent becomes the default. The previously-default
    /// row is demoted in the same transaction.
    #[serde(default)]
    is_default: bool,
    /// MCP servers the new agent may use. Omitted = no MCP access (`[]`):
    /// there is no "unrestricted" mode. The operator opts in explicitly.
    #[serde(default)]
    allowed_mcp_servers: Vec<McpServerId>,
}

#[derive(Debug, Deserialize)]
struct UpdateAgentRequest {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    system_prompt: Option<String>,
    /// Patch the description. `Some(_)` re-embeds; `None` (field omitted)
    /// leaves the existing description and embedding untouched.
    #[serde(default)]
    description: Option<String>,
    /// `Some(true)` promotes this row to default (atomically demotes the
    /// previous default). `Some(false)` is rejected when applied to the
    /// current default — the system requires exactly one default at all times.
    #[serde(default)]
    is_default: Option<bool>,
    /// `Some(list)` replaces the allowlist atomically — including
    /// `Some([])`, the explicit lockdown that revokes every server. `None`
    /// (field omitted) leaves the existing allowlist untouched.
    #[serde(default)]
    allowed_mcp_servers: Option<Vec<McpServerId>>,
}

async fn create_agent(
    State(state): State<AppState>,
    principal: Principal,
    Json(payload): Json<CreateAgentRequest>,
) -> Result<(StatusCode, Json<AgentResponse>), HttpError> {
    let name = AgentName::try_from(payload.name).map_err(HttpError::Parse)?;
    let system_prompt =
        AgentSystemPrompt::try_from(payload.system_prompt).map_err(HttpError::Parse)?;
    let description = AgentDescription::try_from(payload.description).map_err(HttpError::Parse)?;
    let allowed_mcp_servers =
        AllowedMcpServers::try_from(payload.allowed_mcp_servers).map_err(HttpError::Parse)?;
    let record = state
        .agents
        .create(NewAgent {
            org_id: principal.active_org_id,
            name,
            system_prompt,
            description,
            is_default: payload.is_default,
            allowed_mcp_servers,
        })
        .await?;
    Ok((StatusCode::CREATED, Json(record.into())))
}

async fn list_agents(
    State(state): State<AppState>,
    principal: Principal,
) -> Result<Json<Vec<AgentResponse>>, HttpError> {
    // Tenant-scoped read: open a tx, set `app.user_id` via the GUC, and
    // let the `agents_org_isolation` RLS policy do the filtering. Mirrors
    // the mcp_servers route — bypasses the store's privileged read path
    // so the user can see only their own org's rows.
    let mut tx = crate::auth::begin_as(&state.pool, &principal).await?;
    let rows = sqlx::query_as::<_, AgentRowForList>(
        "SELECT id, org_id, name, system_prompt, description, is_default, \
                allowed_mcp_servers, created_at, updated_at \
         FROM agents ORDER BY created_at ASC",
    )
    .fetch_all(&mut *tx)
    .await
    .map_err(AuthError::from)?;
    tx.commit().await.map_err(AuthError::from)?;
    let out = rows
        .into_iter()
        .map(AgentRowForList::into_response)
        .collect();
    Ok(Json(out))
}

async fn read_agent(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<Uuid>,
) -> Result<Json<AgentResponse>, HttpError> {
    let id = AgentId::from(id);
    let mut tx = crate::auth::begin_as(&state.pool, &principal).await?;
    let row = sqlx::query_as::<_, AgentRowForList>(
        "SELECT id, org_id, name, system_prompt, description, is_default, \
                allowed_mcp_servers, created_at, updated_at \
         FROM agents WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(AuthError::from)?;
    tx.commit().await.map_err(AuthError::from)?;
    let row = row.ok_or(HttpError::NotFound)?;
    Ok(Json(row.into_response()))
}

async fn update_agent(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<Uuid>,
    Json(payload): Json<UpdateAgentRequest>,
) -> Result<Json<AgentResponse>, HttpError> {
    let id = AgentId::from(id);
    let name = payload
        .name
        .map(AgentName::try_from)
        .transpose()
        .map_err(HttpError::Parse)?;
    let system_prompt = payload
        .system_prompt
        .map(AgentSystemPrompt::try_from)
        .transpose()
        .map_err(HttpError::Parse)?;
    let description = payload
        .description
        .map(AgentDescription::try_from)
        .transpose()
        .map_err(HttpError::Parse)?;
    let allowed_mcp_servers = payload
        .allowed_mcp_servers
        .map(AllowedMcpServers::try_from)
        .transpose()
        .map_err(HttpError::Parse)?;
    // Tenant gate: 404 cross-org / unknown ids without leaking existence
    // before dispatching the privileged update.
    if !crate::auth::visible_to(
        &state.pool,
        &principal,
        crate::auth::VisibilityTable::Agents,
        id.as_uuid(),
    )
    .await?
    {
        return Err(HttpError::NotFound);
    }
    let row = state
        .agents
        .update(
            id,
            AgentUpdate {
                name,
                system_prompt,
                description,
                is_default: payload.is_default,
                allowed_mcp_servers,
            },
        )
        .await?;
    Ok(Json(row.into()))
}

async fn delete_agent(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, HttpError> {
    let id = AgentId::from(id);
    if !crate::auth::visible_to(
        &state.pool,
        &principal,
        crate::auth::VisibilityTable::Agents,
        id.as_uuid(),
    )
    .await?
    {
        return Err(HttpError::NotFound);
    }
    state.agents.delete(id).await?;
    Ok(StatusCode::NO_CONTENT)
}

// Local row type for the tenant-scoped SELECTs. Mirrors the columns
// returned by the store's read path but lives here so the route can run
// raw SQL inside the principal-scoped tx without going through the
// store's privileged transaction.
#[derive(sqlx::FromRow)]
struct AgentRowForList {
    id: AgentId,
    org_id: crate::auth::OrgId,
    name: String,
    system_prompt: String,
    description: String,
    is_default: bool,
    allowed_mcp_servers: Vec<McpServerId>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl AgentRowForList {
    fn into_response(self) -> AgentResponse {
        let _ = self.org_id; // not on the wire shape — RLS already filtered.
        AgentResponse {
            id: self.id,
            name: self.name,
            system_prompt: self.system_prompt,
            description: self.description,
            is_default: self.is_default,
            allowed_mcp_servers: self.allowed_mcp_servers,
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}
