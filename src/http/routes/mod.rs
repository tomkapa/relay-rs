//! Per-domain route modules. Each submodule exposes a `router()` that returns a
//! `Router<AppState>` for its slice of the wire surface; this module merges them and
//! attaches global middleware once.

mod agents;
mod auth;
mod healthz;
mod mcp;
mod me;
mod memory;
mod prompts;
mod threads;

use axum::Router;
use axum::middleware;
use tower_http::trace::TraceLayer;

use super::auth_layer::require_principal;
use super::csrf::require_csrf;
use super::state::AppState;

pub fn router(state: AppState) -> Router {
    let public = Router::new().merge(auth::router()).merge(healthz::router());

    let private = Router::new()
        .merge(prompts::router())
        .merge(agents::router())
        .merge(mcp::router())
        .merge(memory::router())
        .merge(threads::router())
        .merge(me::router())
        // CSRF guards every state-changing request inside the
        // authenticated subtree. Order matters: it runs AFTER
        // `require_principal` so the public subtree is never reached
        // (OAuth login/callback have no cookie to compare yet).
        .route_layer(middleware::from_fn(require_csrf))
        // route_layer is the only correct place for auth middleware —
        // applying it via `.layer` would also wrap the public subtree
        // below and reject `/auth/google/*` with 401.
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            require_principal,
        ));

    Router::new()
        .merge(public)
        .merge(private)
        .with_state(state)
        .layer(TraceLayer::new_for_http())
}
