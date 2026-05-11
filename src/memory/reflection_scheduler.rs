//! Background scheduler that enqueues reflection turns
//! (doc/memory.md §1.6).
//!
//! Polls Postgres on a configurable cadence. For each `(agent, session)`
//! pair where:
//!
//! 1. the time since the latest message exceeds
//!    [`super::limits::REFLECTION_IDLE_TIMEOUT_SECS`], AND
//! 2. there are messages strictly after the latest
//!    `reflection_checkpoints` row for that pair
//!
//! the scheduler enqueues a single `RequestKind::Reflection` job. The
//! scheduler never talks to the LLM — the worker pool dispatches the
//! resulting row through the same `Agent` path as a normal turn.

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::agents::AgentId;
use crate::clock::SharedClock;
use crate::runtime::{
    IdempotencyKey, NewPromptRequest, PromptRequestId, RequestKind, RequestKindPayload,
    RequestStatus, SharedPromptQueue,
};
use crate::session::SessionId;
use crate::types::{Participant, Prompt};

use super::limits::{
    REFLECTION_IDLE_TIMEOUT_SECS, REFLECTION_SCHEDULER_BATCH_LIMIT, REFLECTION_SCHEDULER_POLL_SECS,
};
use super::scheduled_task::ScheduledTask;

#[derive(Debug)]
pub struct ReflectionScheduler {
    task: ScheduledTask,
}

impl ReflectionScheduler {
    /// Spawn with the production poll cadence. The supplied parent token
    /// wires shutdown into the main runtime Ctrl+C signal.
    #[must_use]
    pub fn spawn(
        pool: PgPool,
        queue: SharedPromptQueue,
        clock: SharedClock,
        parent: CancellationToken,
    ) -> Self {
        Self::spawn_with_cadence(
            pool,
            queue,
            clock,
            Duration::from_secs(REFLECTION_SCHEDULER_POLL_SECS),
            Some(parent),
        )
    }

    /// Spawn with an explicit poll cadence. Tests use this to avoid waiting
    /// the production 60s; production callers use [`Self::spawn`].
    #[must_use]
    pub fn spawn_with_cadence(
        pool: PgPool,
        queue: SharedPromptQueue,
        clock: SharedClock,
        poll_interval: Duration,
        parent: Option<CancellationToken>,
    ) -> Self {
        let inner = Arc::new(SchedulerInner {
            pool,
            queue,
            clock,
            idle_threshold: chrono::Duration::seconds(
                i64::try_from(REFLECTION_IDLE_TIMEOUT_SECS)
                    .expect("invariant: REFLECTION_IDLE_TIMEOUT_SECS fits in i64"),
            ),
            batch_limit: REFLECTION_SCHEDULER_BATCH_LIMIT,
        });
        let task = ScheduledTask::spawn("reflection_scheduler", poll_interval, parent, move || {
            let inner = inner.clone();
            async move { inner.tick().await }
        });
        Self { task }
    }

    pub async fn shutdown(self) {
        self.task.shutdown().await;
    }
}

#[derive(Debug)]
struct SchedulerInner {
    pool: PgPool,
    queue: SharedPromptQueue,
    clock: SharedClock,
    idle_threshold: chrono::Duration,
    batch_limit: usize,
}

impl SchedulerInner {
    async fn tick(&self) -> Result<(), sqlx::Error> {
        let now: DateTime<Utc> = self.clock.now_wall().into();
        let cutoff = now - self.idle_threshold;
        let candidates = self.find_candidates(cutoff).await?;

        for c in candidates {
            if let Err(e) = self.enqueue_reflection(&c).await {
                warn!(
                    error = %e,
                    relay.agent.id = %c.agent_id,
                    relay.session.id = %c.session_id,
                    "reflection_scheduler.enqueue.error",
                );
            } else {
                info!(
                    relay.agent.id = %c.agent_id,
                    relay.session.id = %c.session_id,
                    relay.reflection.since_turn_id = %c.last_turn_id,
                    "reflection_scheduler.enqueued",
                );
            }
        }
        Ok(())
    }

    /// Find `(agent, session)` pairs whose latest message is older than
    /// `cutoff` and which have at least one message past the most recent
    /// reflection checkpoint (or no checkpoint at all). Excludes pairs
    /// that already have a pending/processing reflection so the scheduler
    /// is idempotent across ticks.
    async fn find_candidates(
        &self,
        cutoff: DateTime<Utc>,
    ) -> Result<Vec<ReflectionCandidate>, sqlx::Error> {
        let rows: Vec<(AgentId, SessionId, PromptRequestId, DateTime<Utc>)> = sqlx::query_as(
            "WITH latest_per_session AS (
                 SELECT m.session_id,
                        MAX(m.seq) AS latest_seq,
                        MAX(m.created_at) AS latest_at
                 FROM session_messages m
                 GROUP BY m.session_id
             )
             SELECT s.participant_a_agent_id AS agent_id,
                    s.id AS session_id,
                    sm.request_id AS last_turn_id,
                    l.latest_at
             FROM latest_per_session l
             JOIN sessions s ON s.id = l.session_id
             JOIN session_messages sm
                 ON sm.session_id = l.session_id AND sm.seq = l.latest_seq
             WHERE s.participant_a_kind = 'agent'
               AND l.latest_at <= $1
               AND NOT EXISTS (
                   SELECT 1 FROM reflection_checkpoints rc
                   WHERE rc.agent_id = s.participant_a_agent_id
                     AND rc.session_id = s.id
                     AND rc.created_at >= l.latest_at
               )
               AND NOT EXISTS (
                   SELECT 1 FROM prompt_requests pr
                   WHERE pr.session_id = s.id
                     AND pr.kind = $3
                     AND pr.status IN ($4, $5)
               )
             UNION ALL
             SELECT s.participant_b_agent_id AS agent_id,
                    s.id AS session_id,
                    sm.request_id AS last_turn_id,
                    l.latest_at
             FROM latest_per_session l
             JOIN sessions s ON s.id = l.session_id
             JOIN session_messages sm
                 ON sm.session_id = l.session_id AND sm.seq = l.latest_seq
             WHERE s.participant_b_kind = 'agent'
               AND l.latest_at <= $1
               AND NOT EXISTS (
                   SELECT 1 FROM reflection_checkpoints rc
                   WHERE rc.agent_id = s.participant_b_agent_id
                     AND rc.session_id = s.id
                     AND rc.created_at >= l.latest_at
               )
               AND NOT EXISTS (
                   SELECT 1 FROM prompt_requests pr
                   WHERE pr.session_id = s.id
                     AND pr.kind = $3
                     AND pr.status IN ($4, $5)
               )
             ORDER BY latest_at ASC
             LIMIT $2",
        )
        .bind(cutoff)
        .bind(i64::try_from(self.batch_limit).expect("invariant: batch limit fits in i64"))
        .bind(RequestKind::Reflection)
        .bind(RequestStatus::Pending)
        .bind(RequestStatus::Processing)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(
                |(agent_id, session_id, last_turn_id, latest_at)| ReflectionCandidate {
                    agent_id,
                    session_id,
                    last_turn_id,
                    latest_at,
                },
            )
            .collect())
    }

    /// Enqueue a single reflection job. The idempotency key derives from
    /// `(agent, session, last_turn_id)` so a candidate that survives across
    /// two ticks (because the previous enqueue is still pending) maps back
    /// to the same row.
    async fn enqueue_reflection(
        &self,
        c: &ReflectionCandidate,
    ) -> Result<(), crate::runtime::PromptError> {
        let viewer = Participant::agent(c.agent_id);
        let key = IdempotencyKey::try_from(format!(
            "reflect-{agent}-{session}-{turn}",
            agent = c.agent_id,
            session = c.session_id,
            turn = c.last_turn_id,
        ))
        .expect("invariant: reflection idempotency key fits the cap");
        // Reflection has no user-facing prompt; the column CHECK requires a
        // non-empty body, so this placeholder satisfies it. The actual
        // prompt is built by the worker from session history.
        let content = Prompt::try_from("(reflection)")
            .expect("invariant: reflection content placeholder is valid");
        let req = NewPromptRequest {
            session: Some(c.session_id),
            sender: viewer,
            receiver_agent_id: c.agent_id,
            parent_session: None,
            content,
            idempotency_key: key,
            kind: RequestKind::Reflection,
            kind_payload: RequestKindPayload::Reflection {
                session_id: c.session_id,
                since_turn_id: c.last_turn_id,
            },
        };
        let outcome = self.queue.enqueue(req).await?;
        debug!(
            relay.request.id = %outcome.request_id(),
            "reflection_scheduler.enqueued.row",
        );
        Ok(())
    }
}

#[derive(Debug, Clone)]
struct ReflectionCandidate {
    agent_id: AgentId,
    session_id: SessionId,
    last_turn_id: PromptRequestId,
    /// Latest message timestamp for this candidate — used only by the SQL
    /// ordering and surfaced in tracing; the loop itself does not read it
    /// after `find_candidates` returns.
    #[allow(dead_code)]
    latest_at: DateTime<Utc>,
}

// SQL paths are covered by `tests/reflection_pipeline.rs` against a real
// Postgres. The candidate struct is too trivial to merit pure-unit tests.
