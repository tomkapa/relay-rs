//! Background scheduler that enqueues reflection turns
//! (doc/memory.md §1.6, §2.4 — Phase 4).
//!
//! The scheduler polls Postgres on a configurable cadence. For each
//! `(agent_id, session_id)` pair where:
//!
//! 1. the time since the latest message exceeds
//!    [`super::limits::REFLECTION_IDLE_TIMEOUT_SECS`], AND
//! 2. there are messages strictly after the latest
//!    `reflection_checkpoints` row for that pair
//!
//! it enqueues a single `RequestKind::Reflection` job. The scheduler
//! never talks to the LLM itself — the worker pool dispatches the
//! resulting row to [`crate::agent_core::Agent::reflect`].
//!
//! The owned `JoinHandle` belongs to [`crate::app::Server`] so graceful
//! shutdown waits on it (CLAUDE.md §7 — no floating tasks).

use std::time::Duration;

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use tokio::task::JoinHandle;
use tokio_util::sync::{CancellationToken, DropGuard};
use tracing::{debug, info, warn};

use crate::agents::AgentId;
use crate::clock::SharedClock;
use crate::runtime::{
    IdempotencyKey, NewPromptRequest, PromptRequestId, RequestKind, RequestKindPayload,
    SharedPromptQueue,
};
use crate::session::SessionId;
use crate::types::{Participant, Prompt};

use super::limits::{
    REFLECTION_IDLE_TIMEOUT_SECS, REFLECTION_SCHEDULER_BATCH_LIMIT, REFLECTION_SCHEDULER_POLL_SECS,
};

/// Owned wrapper around the scheduler task. Drop or call
/// [`Self::shutdown`] to wind it down.
pub struct ReflectionScheduler {
    shutdown: DropGuard,
    handle: JoinHandle<()>,
}

impl std::fmt::Debug for ReflectionScheduler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReflectionScheduler")
            .finish_non_exhaustive()
    }
}

impl ReflectionScheduler {
    /// Spawn the background loop with the production poll cadence
    /// ([`REFLECTION_SCHEDULER_POLL_SECS`]).
    #[must_use]
    pub fn spawn(pool: PgPool, queue: SharedPromptQueue, clock: SharedClock) -> Self {
        Self::spawn_with_cadence(
            pool,
            queue,
            clock,
            Duration::from_secs(REFLECTION_SCHEDULER_POLL_SECS),
        )
    }

    /// Spawn with an explicit poll cadence. Used by tests so they don't
    /// have to wait the production 60-second tick; callers in
    /// composition roots use [`Self::spawn`].
    #[must_use]
    pub fn spawn_with_cadence(
        pool: PgPool,
        queue: SharedPromptQueue,
        clock: SharedClock,
        poll_interval: Duration,
    ) -> Self {
        let cancel = CancellationToken::new();
        let token = cancel.clone();
        let inner = SchedulerLoop {
            pool,
            queue,
            clock,
            poll_interval,
            idle_threshold: chrono::Duration::seconds(
                i64::try_from(REFLECTION_IDLE_TIMEOUT_SECS)
                    .expect("invariant: REFLECTION_IDLE_TIMEOUT_SECS fits in i64"),
            ),
            batch_limit: REFLECTION_SCHEDULER_BATCH_LIMIT,
        };
        let handle = tokio::spawn(async move { inner.run(token).await });
        Self {
            shutdown: cancel.drop_guard(),
            handle,
        }
    }

    /// Cancel and join. Idempotent.
    pub async fn shutdown(self) {
        drop(self.shutdown);
        if let Err(e) = self.handle.await {
            warn!(error = %e, "reflection_scheduler.join.error");
        }
    }
}

#[derive(Debug)]
struct SchedulerLoop {
    pool: PgPool,
    queue: SharedPromptQueue,
    clock: SharedClock,
    poll_interval: Duration,
    idle_threshold: chrono::Duration,
    batch_limit: usize,
}

impl SchedulerLoop {
    async fn run(self, cancel: CancellationToken) {
        loop {
            tokio::select! {
                biased;
                () = cancel.cancelled() => return,
                () = tokio::time::sleep(self.poll_interval) => {},
            }
            if let Err(e) = self.tick().await {
                warn!(error = %e, "reflection_scheduler.tick.error");
            }
        }
    }

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

    /// Find `(agent_id, session_id)` pairs whose latest message is older
    /// than `cutoff` AND has at least one message past the most recent
    /// reflection checkpoint (or no checkpoint at all).
    ///
    /// One round-trip per tick. The query joins through
    /// `session_messages` to compute "latest message per (agent,
    /// session)" and against `reflection_checkpoints` to filter out
    /// already-processed pairs.
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
                     AND pr.kind = 'reflection'
                     AND pr.status IN ('pending', 'processing')
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
                     AND pr.kind = 'reflection'
                     AND pr.status IN ('pending', 'processing')
               )
             ORDER BY latest_at ASC
             LIMIT $2",
        )
        .bind(cutoff)
        .bind(i64::try_from(self.batch_limit).expect("invariant: batch limit fits in i64"))
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

    /// Enqueue a single reflection job. Idempotency key derives from
    /// `(agent, session, last_turn_id)` so a candidate that survives
    /// across two ticks (because the previous enqueue is still pending)
    /// returns the same row instead of duplicating.
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
        // The `content` is the user-prompt body. Reflection has no
        // user-facing prompt — we put a short placeholder so the column
        // CHECK passes; the actual prompt is built by `agent.reflect`
        // from the conversation history.
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
            kind_payload: Some(RequestKindPayload::Reflection {
                session_id: c.session_id,
                since_turn_id: c.last_turn_id,
            }),
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
    latest_at: DateTime<Utc>,
}

#[allow(dead_code)]
impl ReflectionCandidate {
    fn debug_age_ms(&self, now: DateTime<Utc>) -> i64 {
        (now - self.latest_at).num_milliseconds()
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    // The scheduler's SQL paths are exercised by the
    // `tests/reflection_scheduler.rs` integration tests against a real
    // Postgres. Pure-unit coverage here would only re-test the trivial
    // candidate-struct accessors.
    #[test]
    fn candidate_age_is_non_negative_for_past_timestamp() {
        let now = Utc::now();
        let earlier = now - chrono::Duration::seconds(120);
        let c = ReflectionCandidate {
            agent_id: AgentId::new(),
            session_id: SessionId::new(),
            last_turn_id: PromptRequestId::new(),
            latest_at: earlier,
        };
        assert!(c.debug_age_ms(now) >= 0);
    }
}
