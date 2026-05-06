//! Postgres-backed [`SessionStore`].
//!
//! Conversation history is stored in `session_messages (session_id, seq, role, body
//! JSONB, created_at)`. The body column carries the full [`ChatMessage`] envelope so
//! adding a content variant is a code change, not a migration. Per-session ordering is
//! provided by the `seq` column, assigned monotonically inside `append`.
//!
//! Wall-clock timestamps come from the injected [`SharedClock`] — never `NOW()` in app
//! SQL — so a `TestClock`-driven test sees stable timestamps (CLAUDE.md §11). Ids
//! cross the SQL boundary via the `sqlx::Type` impl on [`SessionId`].

use std::fmt;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::PgPool;

use crate::agents::AgentId;
use crate::clock::SharedClock;
use crate::provider::ChatMessage;

use super::error::SessionError;
use super::limits::MAX_MESSAGES_PER_SESSION;
use super::traits::{SessionId, SessionStore};

/// Postgres-backed [`SessionStore`]. Holds a cheap clone of a [`PgPool`] and a
/// [`SharedClock`]; safe to share across the runtime.
pub struct PgSessionStore {
    pool: PgPool,
    clock: SharedClock,
    message_cap: u32,
}

impl PgSessionStore {
    /// Construct a store backed by `pool`, using `clock` for every wall-clock value.
    #[must_use]
    pub fn new(pool: PgPool, clock: SharedClock) -> Self {
        Self::with_caps(pool, clock, MAX_MESSAGES_PER_SESSION)
    }

    #[must_use]
    pub fn with_caps(pool: PgPool, clock: SharedClock, message_cap: u32) -> Self {
        Self {
            pool,
            clock,
            message_cap,
        }
    }

    fn now(&self) -> DateTime<Utc> {
        DateTime::<Utc>::from(self.clock.now_wall())
    }
}

impl fmt::Debug for PgSessionStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PgSessionStore")
            .field("message_cap", &self.message_cap)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl SessionStore for PgSessionStore {
    async fn create(&self, agent_id: AgentId) -> Result<SessionId, SessionError> {
        let id = SessionId::new();
        let now = self.now();
        // The FK on `sessions.agent_id` rejects unknown ids with a Postgres
        // 23503 (foreign_key_violation); map it back to the typed
        // `AgentNotFound` so handlers can return a 400 instead of a 500.
        let res =
            sqlx::query("INSERT INTO sessions (id, created_at, agent_id) VALUES ($1, $2, $3)")
                .bind(id)
                .bind(now)
                .bind(agent_id)
                .execute(&self.pool)
                .await;
        match res {
            Ok(_) => Ok(id),
            Err(sqlx::Error::Database(db)) if db.code().as_deref() == Some("23503") => {
                Err(SessionError::AgentNotFound(agent_id))
            }
            Err(e) => Err(e.into()),
        }
    }

    async fn append(&self, id: SessionId, message: ChatMessage) -> Result<(), SessionError> {
        let now = self.now();
        let role = message.role();
        let body = serde_json::to_value(&message).map_err(|e| {
            // §12: serialization failure is a backend invariant, not a user error.
            SessionError::Backend(format!("serialize message: {e}"))
        })?;

        let mut tx = self.pool.begin().await?;

        // Lock the session row first so concurrent appends to the same session
        // serialise; a missing row resolves to NotFound. Postgres does not allow
        // FOR UPDATE on aggregates, so the cap check is a separate query under the
        // same transaction.
        let exists: Option<(SessionId,)> =
            sqlx::query_as("SELECT id FROM sessions WHERE id = $1 FOR UPDATE")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await?;
        if exists.is_none() {
            return Err(SessionError::NotFound(id));
        }

        let (next_seq, row_count): (i64, i64) = sqlx::query_as(
            "SELECT
                COALESCE(MAX(seq) + 1, 0) AS next_seq,
                COUNT(*)                  AS row_count
             FROM session_messages
             WHERE session_id = $1",
        )
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;

        let cap = self.message_cap;
        let cap_i64 = i64::from(cap);
        if row_count >= cap_i64 {
            return Err(SessionError::MessageCapExceeded { id, max: cap });
        }

        sqlx::query(
            "INSERT INTO session_messages (session_id, seq, role, body, created_at)
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(id)
        .bind(next_seq)
        .bind(role)
        .bind(body)
        .bind(now)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(())
    }

    async fn snapshot(&self, id: SessionId) -> Result<Vec<ChatMessage>, SessionError> {
        let exists: Option<(SessionId,)> = sqlx::query_as("SELECT id FROM sessions WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        if exists.is_none() {
            return Err(SessionError::NotFound(id));
        }

        let rows: Vec<(serde_json::Value,)> = sqlx::query_as(
            "SELECT body FROM session_messages WHERE session_id = $1 ORDER BY seq ASC",
        )
        .bind(id)
        .fetch_all(&self.pool)
        .await?;

        let mut out = Vec::with_capacity(rows.len());
        for (body,) in rows {
            let msg: ChatMessage = serde_json::from_value(body).map_err(|e| {
                SessionError::Backend(format!("deserialize message for session {id:?}: {e}"))
            })?;
            out.push(msg);
        }
        Ok(out)
    }

    async fn agent_id(&self, id: SessionId) -> Result<AgentId, SessionError> {
        let row: Option<(AgentId,)> = sqlx::query_as("SELECT agent_id FROM sessions WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        let (agent_id,) = row.ok_or(SessionError::NotFound(id))?;
        Ok(agent_id)
    }

    async fn delete(&self, id: SessionId) -> Result<(), SessionError> {
        let res = sqlx::query("DELETE FROM sessions WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        if res.rows_affected() == 0 {
            return Err(SessionError::NotFound(id));
        }
        Ok(())
    }
}
