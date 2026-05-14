//! Postgres-backed [`ScheduledTaskStore`].
//!
//! All wall-clock values come from the injected [`SharedClock`] — never
//! `NOW()` in app SQL — so a `TestClock`-driven test sees stable
//! `created_at` / `updated_at` (CLAUDE.md §11). The `schedule` JSONB
//! column is serialised via serde at the boundary; nothing downstream
//! sees a raw `serde_json::Value`.

use std::fmt;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::PgPool;

use crate::agents::AgentId;
use crate::clock::SharedClock;
use crate::runtime::PromptRequestId;

use super::error::ScheduledTaskError;
use super::limits::MAX_SCHEDULED_TASKS_PER_AGENT;
use super::store::{NewScheduledTask, ScheduledTaskStore};
use super::types::{
    ScheduledPrompt, ScheduledTaskId, ScheduledTaskName, ScheduledTaskRecord, ScheduledTaskState,
};

pub struct PgScheduledTaskStore {
    pool: PgPool,
    clock: SharedClock,
}

impl PgScheduledTaskStore {
    #[must_use]
    pub fn new(pool: PgPool, clock: SharedClock) -> Self {
        Self { pool, clock }
    }

    fn now(&self) -> DateTime<Utc> {
        DateTime::<Utc>::from(self.clock.now_wall())
    }
}

impl fmt::Debug for PgScheduledTaskStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PgScheduledTaskStore")
            .finish_non_exhaustive()
    }
}

/// SELECT projection used by every read path. Order must match
/// [`row_to_record`].
const SELECT_COLUMNS: &str = "id, owner_agent_id, name, prompt, schedule, \
     next_run_at, last_fired_at, last_request_id, state, created_at, updated_at";

#[allow(clippy::type_complexity)]
type Row = (
    ScheduledTaskId,
    AgentId,
    String,
    String,
    serde_json::Value,
    Option<DateTime<Utc>>,
    Option<DateTime<Utc>>,
    Option<PromptRequestId>,
    ScheduledTaskState,
    DateTime<Utc>,
    DateTime<Utc>,
);

fn row_to_record(row: Row) -> Result<ScheduledTaskRecord, ScheduledTaskError> {
    let (
        id,
        owner_agent_id,
        name,
        prompt,
        schedule,
        next_run_at,
        last_fired_at,
        last_request_id,
        state,
        created_at,
        updated_at,
    ) = row;
    Ok(ScheduledTaskRecord {
        id,
        owner_agent_id,
        name: ScheduledTaskName::try_from(name)?,
        prompt: ScheduledPrompt::try_from(prompt)?,
        schedule: serde_json::from_value(schedule)?,
        next_run_at,
        last_fired_at,
        last_request_id,
        state,
        created_at,
        updated_at,
    })
}

#[async_trait]
impl ScheduledTaskStore for PgScheduledTaskStore {
    async fn create(
        &self,
        payload: NewScheduledTask,
    ) -> Result<ScheduledTaskRecord, ScheduledTaskError> {
        let id = ScheduledTaskId::new();
        let now = self.now();
        let schedule_json = serde_json::to_value(&payload.schedule)?;
        let owner = payload.owner_agent_id;
        let cap_label = format!("relay:sched_cap:{owner}");

        let mut tx = self.pool.begin().await?;
        // Per-agent advisory lock — pg_advisory_xact_lock serializes
        // concurrent create-for-same-owner transactions so the count +
        // insert below cannot race past the cap (READ COMMITTED alone
        // does not prevent the TOCTOU).
        sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1))")
            .bind(&cap_label)
            .execute(&mut *tx)
            .await?;

        let (count,): (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM scheduled_tasks \
             WHERE owner_agent_id = $1 AND state = $2",
        )
        .bind(owner)
        .bind(ScheduledTaskState::Active)
        .fetch_one(&mut *tx)
        .await?;
        let count_usize = usize::try_from(count).map_err(|_| {
            ScheduledTaskError::Db(sqlx::Error::Decode(
                format!("invariant: negative count {count}").into(),
            ))
        })?;
        if count_usize >= MAX_SCHEDULED_TASKS_PER_AGENT {
            return Err(ScheduledTaskError::PerAgentCapExceeded {
                agent: owner,
                max: MAX_SCHEDULED_TASKS_PER_AGENT,
            });
        }

        let row: Row = sqlx::query_as(&format!(
            "INSERT INTO scheduled_tasks \
                ({SELECT_COLUMNS}) \
             VALUES ($1, $2, $3, $4, $5, $6, NULL, NULL, $7, $8, $8) \
             RETURNING {SELECT_COLUMNS}"
        ))
        .bind(id)
        .bind(owner)
        .bind(payload.name.as_str())
        .bind(payload.prompt.as_str())
        .bind(&schedule_json)
        .bind(payload.next_run_at)
        .bind(ScheduledTaskState::Active)
        .bind(now)
        .fetch_one(&mut *tx)
        .await?;

        tx.commit().await?;
        row_to_record(row)
    }

    async fn list_for_agent(
        &self,
        owner: AgentId,
    ) -> Result<Vec<ScheduledTaskRecord>, ScheduledTaskError> {
        // §5: explicit upper bound. The create path enforces
        // MAX_SCHEDULED_TASKS_PER_AGENT, so the LIMIT is a defence in
        // depth against drift in that invariant.
        let cap = i64::try_from(MAX_SCHEDULED_TASKS_PER_AGENT).map_err(|_| {
            ScheduledTaskError::Db(sqlx::Error::Decode(
                format!("invariant: cap {MAX_SCHEDULED_TASKS_PER_AGENT} exceeds i64").into(),
            ))
        })?;
        let rows: Vec<Row> = sqlx::query_as(&format!(
            "SELECT {SELECT_COLUMNS} FROM scheduled_tasks \
             WHERE owner_agent_id = $1 AND state = $2 \
             ORDER BY created_at ASC \
             LIMIT $3"
        ))
        .bind(owner)
        .bind(ScheduledTaskState::Active)
        .bind(cap)
        .fetch_all(&self.pool)
        .await?;
        assert!(
            rows.len() <= MAX_SCHEDULED_TASKS_PER_AGENT,
            "invariant: list_for_agent exceeded per-agent cap"
        );

        rows.into_iter().map(row_to_record).collect()
    }

    async fn cancel(
        &self,
        task: ScheduledTaskId,
        owner: AgentId,
    ) -> Result<(), ScheduledTaskError> {
        let now = self.now();
        let mut tx = self.pool.begin().await?;

        // Lock by `(id, owner_agent_id)` — a cross-owner caller sees
        // zero rows here, which folds into `NotFound` at the bottom.
        let existing: Option<(ScheduledTaskState,)> = sqlx::query_as(
            "SELECT state FROM scheduled_tasks \
             WHERE id = $1 AND owner_agent_id = $2 FOR UPDATE",
        )
        .bind(task)
        .bind(owner)
        .fetch_optional(&mut *tx)
        .await?;

        let Some((state,)) = existing else {
            return Err(ScheduledTaskError::NotFound(task));
        };
        if !matches!(state, ScheduledTaskState::Active) {
            // Already cancelled / done — idempotent no-op.
            tx.commit().await?;
            return Ok(());
        }

        sqlx::query(
            "UPDATE scheduled_tasks \
             SET state = $1, next_run_at = NULL, updated_at = $2 \
             WHERE id = $3",
        )
        .bind(ScheduledTaskState::Cancelled)
        .bind(now)
        .bind(task)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(())
    }

    async fn claim_due(
        &self,
        now: DateTime<Utc>,
        limit: usize,
    ) -> Result<Vec<ScheduledTaskRecord>, ScheduledTaskError> {
        // Plain SELECT — no row-level lock. Concurrent scheduler nodes
        // (future) dedupe at the queue layer via the
        // `sched-{task_id}-{fire_ts}` idempotency key; `record_fired`
        // is idempotent over the same `(fired_at, next_run_at)` pair.
        let rows: Vec<Row> = sqlx::query_as(&format!(
            "SELECT {SELECT_COLUMNS} FROM scheduled_tasks \
             WHERE state = $1 \
               AND next_run_at IS NOT NULL \
               AND next_run_at <= $2 \
             ORDER BY next_run_at ASC \
             LIMIT $3"
        ))
        .bind(ScheduledTaskState::Active)
        .bind(now)
        .bind(i64::try_from(limit).map_err(|_| {
            ScheduledTaskError::Db(sqlx::Error::Decode(
                format!("invariant: limit {limit} exceeds i64").into(),
            ))
        })?)
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter().map(row_to_record).collect()
    }

    async fn record_fired(
        &self,
        task: ScheduledTaskId,
        request_id: PromptRequestId,
        fired_at: DateTime<Utc>,
        next_run_at: Option<DateTime<Utc>>,
    ) -> Result<(), ScheduledTaskError> {
        let now = self.now();
        // `next_run_at IS NULL` ⇒ schedule exhausted ⇒ flip state to Done.
        let next_state = if next_run_at.is_some() {
            ScheduledTaskState::Active
        } else {
            ScheduledTaskState::Done
        };
        sqlx::query(
            "UPDATE scheduled_tasks \
             SET last_fired_at = $1, \
                 last_request_id = $2, \
                 next_run_at = $3, \
                 state = $4, \
                 updated_at = $5 \
             WHERE id = $6",
        )
        .bind(fired_at)
        .bind(request_id)
        .bind(next_run_at)
        .bind(next_state)
        .bind(now)
        .bind(task)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}
