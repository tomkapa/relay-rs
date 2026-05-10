//! Agents registry — multi-agent support.
//!
//! Each row in the `agents` table is one persona: a display name plus a
//! role-specific system prompt. The composition root seeds a single default
//! agent at startup ([`PgAgentStore::seed_default`]); session-create binds a
//! session to either an explicitly-supplied `agent_id` or the default. Every
//! turn loads the agent's prompt through [`AgentPromptCache`] (60s TTL) and
//! assembles the final `system` field as
//! `<core>...</core>\n<role>{prompt}</role>` — see
//! [`crate::memory::AgentMemory`] for the assembly seam.
//!
//! Distinct from [`crate::agent_core`]: that module owns the runtime orchestrator
//! (`Agent`, the chat loop). This one owns the *registry* of agent definitions.

mod cache;
mod error;
mod limits;
mod pg_store;
mod registry;
mod store;
mod types;

pub use cache::AgentPromptCache;
pub use error::AgentStoreError;
pub use limits::{
    AGENT_NAME_MAX_LEN, AGENT_PROMPT_CACHE_CAP, AGENT_PROMPT_CACHE_TTL, AGENT_SYSTEM_PROMPT_MAX_LEN,
};
pub use pg_store::PgAgentStore;
pub use registry::{AgentFactory, Agents, AgentsError, CachedAgents, SharedAgents};
pub use store::{AgentStore, AgentUpdate, NewAgent, SharedAgentStore};
pub use types::{AgentId, AgentName, AgentRecord, AgentSystemPrompt, DefaultAgentSeed};
