//! Session abstraction.
//!
//! A session owns the conversation history of a single agent interaction. The agent
//! never holds a `Vec<ChatMessage>` directly — it asks a `SessionStore` for a snapshot
//! before each turn and appends new messages back. Persistence (in-memory, Postgres,
//! Redis, S3) is a matter of adding another impl of [`SessionStore`].

mod error;
mod limits;
mod store;

pub use error::SessionError;
pub use limits::{MAX_MESSAGES_PER_SESSION, MAX_SESSIONS_DEFAULT};
pub use store::{InMemorySessionStore, SessionId, SessionStore, SharedSessionStore};
