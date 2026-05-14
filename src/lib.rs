//! Relay-rs — provider-agnostic, hookable agent runtime.
//!
//! The seams (`provider`, `session`, `memory`, `hook`, `tools`) are the public surface.
//! `Agent` orchestrates them; nothing else does. Adding a new backend on any seam means
//! one new module and one composition-root edit in [`app::build_agent`].

pub mod agent_core;
pub mod agents;
pub mod app;
pub mod cache;
pub mod clock;
pub mod config;
pub mod error;
pub mod hook;
pub mod http;
pub mod mcp;
pub mod memory;
pub mod observability;
pub mod provider;
pub mod runtime;
pub mod session;
pub mod tools;
pub mod types;

pub use agent_core::{Agent, AgentBuilder, AgentError};
pub use agents::{AgentId, AgentRecord, AgentStore, SharedAgentStore};
pub use config::{ProviderSettings, Settings, SettingsError};
pub use error::AppError;
