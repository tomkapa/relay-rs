//! Postgres-backed [`AgentStore`].
//!
//! Wall-clock values come from the injected [`SharedClock`] — never `NOW()` in app
//! SQL — so a `TestClock`-driven test sees stable timestamps (CLAUDE.md §11). Ids
//! cross the SQL boundary via the macro-generated `sqlx::Type` impl on
//! [`AgentId`].

use std::fmt;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::PgPool;

use crate::clock::SharedClock;

use super::error::AgentStoreError;
use super::store::AgentStore;
use super::types::{AgentId, AgentName, AgentRecord, AgentSystemPrompt, DefaultAgentSeed};

/// Transaction-scoped advisory-lock key used by [`PgAgentStore::seed_default`] to
/// serialise its "check default exists, insert if not" critical section across
/// concurrent app starts. Released automatically on commit/rollback. Literal is
/// `0x6167656E745F6473` (= ASCII "agent_ds") — chosen for readability and to
/// avoid colliding with the MCP create lock.
const AGENT_DEFAULT_SEED_LOCK_KEY: i64 = 0x6167_656E_745F_6473;

/// Postgres-backed [`AgentStore`]. Holds a cheap clone of a [`PgPool`] and a
/// [`SharedClock`]; safe to share across the runtime.
pub struct PgAgentStore {
    pool: PgPool,
    clock: SharedClock,
}

impl PgAgentStore {
    #[must_use]
    pub fn new(pool: PgPool, clock: SharedClock) -> Self {
        Self { pool, clock }
    }

    fn now(&self) -> DateTime<Utc> {
        DateTime::<Utc>::from(self.clock.now_wall())
    }

    /// Idempotent seed: insert `seed` as the default agent if no default row
    /// exists. Returns the id of the resulting default row, whether minted here
    /// or already present.
    ///
    /// Concurrent app starts are serialised by a transaction-scoped advisory
    /// lock; the partial unique index `agents_default_unique` is the last line
    /// of defence if the lock is bypassed.
    pub async fn seed_default(&self, seed: DefaultAgentSeed) -> Result<AgentId, AgentStoreError> {
        let now = self.now();
        let mut tx = self.pool.begin().await?;

        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(AGENT_DEFAULT_SEED_LOCK_KEY)
            .execute(&mut *tx)
            .await?;

        let existing: Option<(AgentId,)> =
            sqlx::query_as("SELECT id FROM agents WHERE is_default = TRUE")
                .fetch_optional(&mut *tx)
                .await?;
        if let Some((id,)) = existing {
            tx.commit().await?;
            return Ok(id);
        }

        let id = AgentId::new();
        sqlx::query(
            "INSERT INTO agents \
                 (id, name, system_prompt, is_default, created_at, updated_at) \
             VALUES ($1, $2, $3, TRUE, $4, $4)",
        )
        .bind(id)
        .bind(seed.name.as_str())
        .bind(seed.system_prompt.as_str())
        .bind(now)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(id)
    }
}

impl fmt::Debug for PgAgentStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PgAgentStore").finish_non_exhaustive()
    }
}

#[async_trait]
impl AgentStore for PgAgentStore {
    async fn read(&self, id: AgentId) -> Result<AgentRecord, AgentStoreError> {
        let row: Option<AgentRow> = sqlx::query_as::<_, AgentRow>(
            "SELECT id, name, system_prompt, is_default, created_at, updated_at \
             FROM agents WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        let row = row.ok_or(AgentStoreError::NotFound(id))?;
        row.try_into()
    }

    async fn default_id(&self) -> Result<AgentId, AgentStoreError> {
        let row: Option<(AgentId,)> =
            sqlx::query_as("SELECT id FROM agents WHERE is_default = TRUE")
                .fetch_optional(&self.pool)
                .await?;
        let (id,) = row.ok_or(AgentStoreError::NoDefault)?;
        Ok(id)
    }
}

#[derive(sqlx::FromRow)]
struct AgentRow {
    id: AgentId,
    name: String,
    system_prompt: String,
    is_default: bool,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl TryFrom<AgentRow> for AgentRecord {
    type Error = AgentStoreError;

    fn try_from(row: AgentRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: row.id,
            name: AgentName::try_from(row.name)?,
            system_prompt: AgentSystemPrompt::try_from(row.system_prompt)?,
            is_default: row.is_default,
            created_at: row.created_at,
            updated_at: row.updated_at,
        })
    }
}
