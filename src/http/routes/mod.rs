//! Per-domain route modules. Each submodule exposes a `router()` that returns a
//! `Router<AppState>` for its slice of the wire surface; this module merges them and
//! attaches global middleware once.

mod agents;
mod mcp;
mod prompts;

use axum::Router;
use tower_http::trace::TraceLayer;

use super::state::AppState;

pub fn router(state: AppState) -> Router {
    Router::new()
        .merge(prompts::router())
        .merge(agents::router())
        .merge(mcp::router())
        .with_state(state)
        .layer(TraceLayer::new_for_http())
}
