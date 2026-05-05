//! Relay-rs — provider-agnostic, hookable agent runtime.
//!
//! The seams (`provider`, `session`, `memory`, `hook`, `tools`) are the public surface.
//! `Agent` orchestrates them; nothing else does. Adding a new backend on any seam means
//! one new module and one composition-root edit in [`app::build_agent`].

pub mod agent;
pub mod app;
pub mod clock;
pub mod config;
pub mod error;
pub mod hook;
pub mod http;
pub mod memory;
pub mod observability;
pub mod provider;
pub mod runtime;
pub mod session;
pub mod tools;
pub mod types;

pub use agent::{Agent, AgentBuilder, AgentError};
pub use config::{ProviderSettings, Settings, SettingsError};
pub use error::AppError;
