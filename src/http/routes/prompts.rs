//! Prompt and request endpoints:
//! * `POST /prompts` — submit a prompt; creates a session lazily on first call
//! * `POST /requests/{id}/cancel` — request cancellation
//!
//! Sessions are created lazily by the queue: the first POST without a
//! `session_id` mints a new conversation; subsequent POSTs pass the
//! `session_id` returned from the response. There is no separate
//! `POST /sessions` — that intermediate step is gone with the multi-agent
//! schema (see `migrations/00000000000004_multi_agent_comm.up.sql`).
//!
//! Per-request SSE (`GET /requests/{id}/stream`) is gone — the chat UI uses
//! the DAG-wide stream at `GET /threads/{root}/stream`. See
//! `doc/backend_plan.md` for the rationale.

use axum::Json;
use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::post;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::agents::AgentId;
use crate::runtime::{
    EnqueueOutcome, IdempotencyKey, NewPromptRequest, PromptRequestId, RequestStatus,
};
use crate::session::SessionId;
use crate::types::{Participant, ParticipantKind, Prompt};

use super::super::error::HttpError;
use super::super::state::AppState;

pub(super) fn router() -> Router<AppState> {
    Router::new()
        .route("/prompts", post(submit_prompt))
        .route("/requests/{id}/cancel", post(cancel_request))
}

#[derive(Debug, Deserialize)]
struct SubmitPromptRequest {
    /// Continuing an existing conversation — omit for the first prompt.
    #[serde(default)]
    session_id: Option<SessionId>,
    /// Which agent should handle this prompt. Omit to bind the new conversation
    /// to the seeded default agent. Ignored when `session_id` is `Some` —
    /// the existing session's receiver agent is preserved.
    #[serde(default)]
    agent_id: Option<AgentId>,
    content: String,
    idempotency_key: String,
}

#[derive(Debug, Serialize)]
struct SubmitPromptResponse {
    request_id: PromptRequestId,
    session_id: SessionId,
    status: RequestStatus,
}

async fn submit_prompt(
    State(state): State<AppState>,
    Json(payload): Json<SubmitPromptRequest>,
) -> Result<(StatusCode, Json<SubmitPromptResponse>), HttpError> {
    let content =
        Prompt::try_from(payload.content).map_err(|e| HttpError::BadRequest(e.to_string()))?;
    let idempotency_key = IdempotencyKey::try_from(payload.idempotency_key)
        .map_err(|e| HttpError::BadRequest(e.to_string()))?;

    // Resolve the receiver agent. Continuing an existing session must always
    // route to that session's agent participant — the worker rejects a
    // prompt whose receiver isn't a participant of the named session
    // ("agent X is not a participant of session Y"). This keeps the comment
    // on `SubmitPromptRequest::agent_id` honest: when `session_id` is set,
    // any caller-supplied `agent_id` is ignored. Only fresh sessions consult
    // the request payload (or fall back to the seeded default).
    let receiver_agent_id = match payload.session_id {
        Some(session_id) => session_agent_participant(&state, session_id).await?,
        None => match payload.agent_id {
            Some(id) => id,
            None => state.agents.default_id().await?,
        },
    };

    let outcome = state
        .queue
        .enqueue(NewPromptRequest {
            session: payload.session_id,
            sender: Participant::Human,
            receiver_agent_id,
            parent_session: None,
            content,
            idempotency_key,
        })
        .await?;

    let status_code = match outcome {
        EnqueueOutcome::Inserted { .. } => StatusCode::ACCEPTED,
        EnqueueOutcome::Existing { .. } => StatusCode::OK,
    };
    Ok((
        status_code,
        Json(SubmitPromptResponse {
            request_id: outcome.request_id(),
            session_id: outcome.session(),
            status: outcome.status(),
        }),
    ))
}

/// Read the agent participant of `session_id`. Human-rooted DAGs always
/// have exactly one Human and one Agent participant; an unexpected pair
/// (Agent-Agent or Human-Human) would be a backend invariant violation
/// for a human-rooted thread, and surfaces as `Internal`.
async fn session_agent_participant(
    state: &AppState,
    session_id: SessionId,
) -> Result<AgentId, HttpError> {
    let (a, b) = state.sessions.participants(session_id).await?;
    match (a.kind(), b.kind()) {
        (ParticipantKind::Agent, _) => Ok(a.agent_id().expect("invariant: agent kind has id")),
        (_, ParticipantKind::Agent) => Ok(b.agent_id().expect("invariant: agent kind has id")),
        _ => Err(HttpError::Internal),
    }
}

async fn cancel_request(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, HttpError> {
    let request_id = PromptRequestId::from(id);
    state.queue.request_cancellation(request_id).await?;
    Ok(StatusCode::NO_CONTENT)
}
