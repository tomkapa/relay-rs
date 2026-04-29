//! Axum routes:
//! * `POST /sessions` → `{ session_id }`
//! * `POST /prompts` `{ session_id, content, idempotency_key }` → `{ request_id, status }`
//! * `GET /requests/:id/stream` → SSE
//! * `POST /requests/:id/cancel` → 204

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
use tower_http::trace::TraceLayer;
use tracing::warn;
use uuid::Uuid;

use crate::runtime::{
    ChunkSeq, EnqueueOutcome, IdempotencyKey, NewPromptRequest, PromptRequestId, RequestStatus,
    ResponseChunk, StreamEvent,
};
use crate::session::SessionId;
use crate::types::Prompt;

use super::error::HttpError;
use super::state::AppState;

const SSE_KEEPALIVE_INTERVAL: Duration = Duration::from_secs(15);

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/sessions", post(create_session))
        .route("/prompts", post(submit_prompt))
        .route("/requests/{id}/stream", get(stream_request))
        .route("/requests/{id}/cancel", post(cancel_request))
        .with_state(state)
        .layer(TraceLayer::new_for_http())
}

#[derive(Debug, Serialize)]
struct CreateSessionResponse {
    session_id: Uuid,
}

async fn create_session(
    State(state): State<AppState>,
) -> Result<Json<CreateSessionResponse>, HttpError> {
    let id = state.sessions.create().await?;
    Ok(Json(CreateSessionResponse {
        session_id: id.as_uuid(),
    }))
}

#[derive(Debug, Deserialize)]
struct SubmitPromptRequest {
    session_id: Uuid,
    content: String,
    idempotency_key: String,
}

#[derive(Debug, Serialize)]
struct SubmitPromptResponse {
    request_id: Uuid,
    status: RequestStatus,
}

async fn submit_prompt(
    State(state): State<AppState>,
    Json(payload): Json<SubmitPromptRequest>,
) -> Result<(StatusCode, Json<SubmitPromptResponse>), HttpError> {
    let session = SessionId::from_uuid(payload.session_id);
    let content =
        Prompt::try_from(payload.content).map_err(|e| HttpError::BadRequest(e.to_string()))?;
    let idempotency_key = IdempotencyKey::try_from(payload.idempotency_key)
        .map_err(|e| HttpError::BadRequest(e.to_string()))?;

    let outcome = state
        .queue
        .enqueue(NewPromptRequest {
            session,
            content,
            idempotency_key,
        })
        .await?;

    let status = match outcome {
        EnqueueOutcome::Inserted { .. } => StatusCode::ACCEPTED,
        EnqueueOutcome::Existing { .. } => StatusCode::OK,
    };
    Ok((
        status,
        Json(SubmitPromptResponse {
            request_id: outcome.request_id().as_uuid(),
            status: outcome.status(),
        }),
    ))
}

async fn cancel_request(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, HttpError> {
    let request_id = PromptRequestId::from_uuid(id);
    state.queue.request_cancellation(request_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn stream_request(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, HttpError> {
    let request_id = PromptRequestId::from_uuid(id);
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
