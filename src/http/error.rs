use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;
use thiserror::Error;

use crate::mcp::McpError;
use crate::runtime::{PromptError, ResponseError};
use crate::session::SessionError;
use crate::types::ParseError;

/// One error type for the HTTP boundary. CLAUDE.md §12: `IntoResponse` lives next to
/// the variants so the HTTP mapping cannot drift from the variant set.
#[derive(Debug, Error)]
pub enum HttpError {
    #[error("invalid request: {0}")]
    BadRequest(String),

    #[error("not found")]
    NotFound,

    #[error("conflict: {0}")]
    Conflict(String),

    #[error("payload too large")]
    PayloadTooLarge,

    #[error("parse: {0}")]
    Parse(#[from] ParseError),

    #[error("session: {0}")]
    Session(#[from] SessionError),

    #[error("prompt pipeline: {0}")]
    Prompt(#[from] PromptError),

    #[error("response stream: {0}")]
    Response(#[from] ResponseError),

    #[error("mcp: {0}")]
    Mcp(#[from] McpError),

    #[error("internal error")]
    Internal,
}

impl IntoResponse for HttpError {
    fn into_response(self) -> Response {
        let (status, message) = match &self {
            Self::BadRequest(m) => (StatusCode::BAD_REQUEST, m.clone()),
            Self::NotFound => (StatusCode::NOT_FOUND, "not found".into()),
            Self::Conflict(m) => (StatusCode::CONFLICT, m.clone()),
            Self::PayloadTooLarge => (StatusCode::PAYLOAD_TOO_LARGE, "too large".into()),
            Self::Parse(e) => (StatusCode::BAD_REQUEST, e.to_string()),
            Self::Session(SessionError::NotFound(_)) => {
                (StatusCode::NOT_FOUND, "session not found".into())
            }
            Self::Session(SessionError::MessageCapExceeded { .. })
            | Self::Prompt(PromptError::PendingCapExceeded { .. })
            | Self::Mcp(McpError::ServerCapExceeded { .. }) => {
                (StatusCode::TOO_MANY_REQUESTS, self.to_string())
            }
            Self::Session(e) => (StatusCode::BAD_REQUEST, e.to_string()),
            Self::Prompt(PromptError::RequestNotFound(_) | PromptError::SessionNotFound(_)) => {
                (StatusCode::NOT_FOUND, "not found".into())
            }
            Self::Prompt(e) => (StatusCode::BAD_REQUEST, e.to_string()),
            Self::Response(ResponseError::NotFound(_)) => {
                (StatusCode::NOT_FOUND, "stream not found".into())
            }
            Self::Response(e) => (StatusCode::BAD_GATEWAY, e.to_string()),
            Self::Mcp(McpError::NotFound(_)) => {
                (StatusCode::NOT_FOUND, "mcp server not found".into())
            }
            Self::Mcp(McpError::AliasTaken(_)) => (StatusCode::CONFLICT, self.to_string()),
            Self::Mcp(McpError::Parse(_) | McpError::InvalidConfig(_)) => {
                (StatusCode::BAD_REQUEST, self.to_string())
            }
            Self::Mcp(_) => (StatusCode::INTERNAL_SERVER_ERROR, "mcp store error".into()),
            Self::Internal => (StatusCode::INTERNAL_SERVER_ERROR, "internal error".into()),
        };
        (status, Json(json!({ "error": message }))).into_response()
    }
}
