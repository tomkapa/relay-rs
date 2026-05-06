//! `POST /sessions` — create a session bound to an agent (defaulting to the seeded
//! default agent when the caller omits `agent_id`).

use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::routing::post;
use serde::{Deserialize, Serialize};

use crate::agents::AgentId;
use crate::session::SessionId;

use super::super::error::HttpError;
use super::super::state::AppState;

pub(super) fn router() -> Router<AppState> {
    Router::new().route("/sessions", post(create_session))
}

#[derive(Debug, Default, Deserialize)]
struct CreateSessionRequest {
    /// Optional. When omitted, the server binds the new session to the default
    /// agent (the row seeded with `is_default = TRUE`).
    #[serde(default)]
    agent_id: Option<AgentId>,
}

#[derive(Debug, Serialize)]
struct CreateSessionResponse {
    session_id: SessionId,
    /// Always populated, even when the client omitted the field — so the
    /// caller knows which agent ended up bound to the session.
    agent_id: AgentId,
}

async fn create_session(
    State(state): State<AppState>,
    Json(body): Json<CreateSessionRequest>,
) -> Result<Json<CreateSessionResponse>, HttpError> {
    let agent_id = match body.agent_id {
        Some(id) => id,
        None => state.agents.default_id().await?,
    };
    // FK on `sessions.agent_id` rejects unknown ids; `PgSessionStore::create`
    // maps that to `SessionError::AgentNotFound`, which `HttpError` already
    // turns into a 400.
    let session_id = state.sessions.create(agent_id).await?;
    Ok(Json(CreateSessionResponse {
        session_id,
        agent_id,
    }))
}
