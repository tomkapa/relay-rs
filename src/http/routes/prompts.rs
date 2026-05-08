//! Prompt and request endpoints:
//! * `POST /prompts` — submit a prompt; creates a session lazily on first call
//! * `POST /requests/{id}/cancel` — request cancellation
//! * `GET  /requests/{id}/stream` — SSE stream of response chunks
//!
//! Sessions are created lazily by the queue: the first POST without a
//! `session_id` mints a new conversation; subsequent POSTs pass the
//! `session_id` returned from the response. There is no separate
//! `POST /sessions` — that intermediate step is gone with the multi-agent
//! schema (see `migrations/00000000000004_multi_agent_comm.up.sql`).

use std::convert::Infallible;
use std::time::Duration;

use axum::Json;
use axum::Router;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::routing::{get, post};
use futures::stream::{Stream, StreamExt};
use serde::{Deserialize, Serialize};
use tracing::warn;
use uuid::Uuid;

use crate::agents::AgentId;
use crate::runtime::{
    ChunkSeq, EnqueueOutcome, IdempotencyKey, NewPromptRequest, PromptRequestId, RequestStatus,
    ResponseChunk, StreamEvent,
};
use crate::session::SessionId;
use crate::types::{Participant, Prompt};

use super::super::error::HttpError;
use super::super::state::AppState;

const SSE_KEEPALIVE_INTERVAL: Duration = Duration::from_secs(15);

pub(super) fn router() -> Router<AppState> {
    Router::new()
        .route("/prompts", post(submit_prompt))
        .route("/requests/{id}/stream", get(stream_request))
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

    let receiver_agent_id = match payload.agent_id {
        Some(id) => id,
        None => state.agents.default_id().await?,
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

async fn cancel_request(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, HttpError> {
    let request_id = PromptRequestId::from(id);
    state.queue.request_cancellation(request_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn stream_request(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, HttpError> {
    let request_id = PromptRequestId::from(id);
    let since = headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
        .map(ChunkSeq::from);

    let inner = state.responses.subscribe(request_id, since).await?;
    let stream = inner.map(|item| {
        let event = match item {
            Ok(StreamEvent::Chunk(env)) => chunk_to_sse(env.seq, &env.chunk),
            Ok(StreamEvent::Stalled) => stalled_event(),
            Err(e) => {
                warn!(error = %e, "sse.stream.error");
                error_event(&e.to_string())
            }
        };
        Ok::<_, Infallible>(event)
    });

    Ok(Sse::new(stream).keep_alive(KeepAlive::new().interval(SSE_KEEPALIVE_INTERVAL)))
}

fn chunk_to_sse(seq: ChunkSeq, chunk: &ResponseChunk) -> Event {
    // Wire format and `event:` name both come from the chunk type itself — see
    // `ResponseChunk` in `runtime/response.rs`.
    let body = serde_json::to_string(chunk)
        .expect("invariant: ResponseChunk's Serialize impl is total over closed enum");
    Event::default()
        .id(seq.get().to_string())
        .event(chunk.event_kind())
        .data(body)
}

fn stalled_event() -> Event {
    chunk_to_sse(ChunkSeq::ZERO, &ResponseChunk::Stalled)
}

fn error_event(message: &str) -> Event {
    chunk_to_sse(
        ChunkSeq::ZERO,
        &ResponseChunk::Error {
            reason: message.into(),
        },
    )
}
