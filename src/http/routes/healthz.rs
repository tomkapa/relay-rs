//! Public liveness probe. Stays outside the auth layer so monitoring
//! agents can hit it without a cookie.

use axum::Router;
use axum::http::StatusCode;
use axum::routing::get;

use super::super::state::AppState;

pub(super) fn router() -> Router<AppState> {
    Router::new().route("/healthz", get(|| async { StatusCode::OK }))
}
