//! HTTP surface (axum). Tower's `TraceLayer` opens a root span per request per
//! CLAUDE.md §2; handlers stay thin and call into `prompt::*` traits.

mod error;
mod routes;
mod state;

pub use error::HttpError;
pub use routes::router;
pub use state::AppState;
