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

use crate::agents::{AgentId, AgentName, AgentRecord, AgentSystemPrompt, AgentUpdate, NewAgent};

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
    /// `null` when the agent has no role-specific reflection guidance —
    /// reflection turns then run with the default reflection-core prompt
    /// alone.
    reflection_role: Option<String>,
    is_default: bool,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl From<AgentRecord> for AgentResponse {
    fn from(r: AgentRecord) -> Self {
        Self {
            id: r.id,
            name: r.name.as_str().to_owned(),
            system_prompt: r.system_prompt.as_str().to_owned(),
            reflection_role: r.reflection_role.as_ref().map(|p| p.as_str().to_owned()),
            is_default: r.is_default,
            created_at: r.created_at,
            updated_at: r.updated_at,
        }
    }
}

#[derive(Debug, Deserialize)]
struct CreateAgentRequest {
    name: String,
    system_prompt: String,
    /// Optional per-agent reflection guidance (doc/memory.md §1.6).
    /// Omit (or send `null` / empty string) to leave reflection on the
    /// default core prompt alone.
    #[serde(default)]
    reflection_role: Option<String>,
    /// When `true`, the new agent becomes the default. The previously-default
    /// row is demoted in the same transaction.
    #[serde(default)]
    is_default: bool,
}

#[derive(Debug, Deserialize)]
struct UpdateAgentRequest {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    system_prompt: Option<String>,
    /// PATCH semantics: omit to leave alone, send a non-empty string to
    /// set, send an empty string to clear back to `null`. Wire shape
    /// trade-off — JSON does not distinguish "missing" from "null"
    /// without a custom deserializer.
    #[serde(default)]
    reflection_role: Option<String>,
    /// `Some(true)` promotes this row to default (atomically demotes the
    /// previous default). `Some(false)` is rejected when applied to the
    /// current default — the system requires exactly one default at all times.
    #[serde(default)]
    is_default: Option<bool>,
}

async fn create_agent(
    State(state): State<AppState>,
    Json(payload): Json<CreateAgentRequest>,
) -> Result<(StatusCode, Json<AgentResponse>), HttpError> {
    let name = AgentName::try_from(payload.name).map_err(HttpError::Parse)?;
    let system_prompt =
        AgentSystemPrompt::try_from(payload.system_prompt).map_err(HttpError::Parse)?;
    let reflection_role = payload
        .reflection_role
        .filter(|s| !s.is_empty())
        .map(AgentSystemPrompt::try_from)
        .transpose()
        .map_err(HttpError::Parse)?;
    let record = state
        .agents
        .create(NewAgent {
            name,
            system_prompt,
            reflection_role,
            is_default: payload.is_default,
        })
        .await?;
    Ok((StatusCode::CREATED, Json(record.into())))
}

async fn list_agents(State(state): State<AppState>) -> Result<Json<Vec<AgentResponse>>, HttpError> {
    let rows = state.agents.list().await?;
    Ok(Json(rows.into_iter().map(AgentResponse::from).collect()))
}

async fn read_agent(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<AgentResponse>, HttpError> {
    let id = AgentId::from(id);
    let row = state.agents.read(id).await?;
    Ok(Json(row.into()))
}

async fn update_agent(
    State(state): State<AppState>,
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
    // Empty string means "clear back to NULL"; absent means "leave alone".
    let reflection_role = match payload.reflection_role {
        None => None,
        Some(s) if s.is_empty() => Some(None),
        Some(s) => Some(Some(
            AgentSystemPrompt::try_from(s).map_err(HttpError::Parse)?,
        )),
    };
    let row = state
        .agents
        .update(
            id,
            AgentUpdate {
                name,
                system_prompt,
                reflection_role,
                is_default: payload.is_default,
            },
        )
        .await?;
    Ok(Json(row.into()))
}

async fn delete_agent(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, HttpError> {
    let id = AgentId::from(id);
    state.agents.delete(id).await?;
    Ok(StatusCode::NO_CONTENT)
}
