//! Per-domain route modules. Each submodule exposes a `router()` that returns a
//! `Router<AppState>` for its slice of the wire surface; this module merges them and
//! attaches global middleware once.

mod agents;
mod mcp;
mod memory;
mod prompts;
mod threads;

use axum::Router;
use tower_http::trace::TraceLayer;

use super::state::AppState;

pub fn router(state: AppState) -> Router {
    Router::new()
        .merge(prompts::router())
        .merge(agents::router())
        .merge(mcp::router())
        .merge(memory::router())
        .merge(threads::router())
        .with_state(state)
        .layer(TraceLayer::new_for_http())
}
