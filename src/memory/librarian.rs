//! Librarian — mechanical sweep + resolution-job enqueue
//! (doc/memory.md §1.8, §2.6, §2.7 — Phases 6 and 7).
//!
//! The mechanical sweep ([`run_librarian_sweep`]) runs no LLM:
//!
//! 1. *Dedup* — pairs above [`DEDUP_SIMILARITY_THRESHOLD`] merge into the
//!    higher-state, older-provenance copy. The other side is forgotten;
//!    a `cross_session_rewrite` validation event lands on the survivor.
//! 2. *Decay* — [`MemoryStore::decay_validated`] demotes Validated rows
//!    whose `last_validated_at` is older than [`VALIDATION_DECAY_SECS`].
//! 3. *Eviction* — when over [`MAX_MEMORIES_PER_AGENT`], the lowest-scored
//!    non-pinned rows are forgotten.
//! 4. *Contradiction detection* — pairs at
//!    [`CONTRADICTION_SIMILARITY_THRESHOLD`] with opposing textual
//!    signals get a `contradiction_events` row.
//!
//! [`LibrarianScheduler`] runs the sweep periodically per agent and, for
//! each unresolved contradiction event, enqueues a `RequestKind::Resolution`
//! job — Phase 7 dispatches that to `agent.resolve_contradiction`.

use std::time::Duration;

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use tokio::task::JoinHandle;
use tokio_util::sync::{CancellationToken, DropGuard};
use tracing::{debug, info, warn};

use crate::agents::AgentId;
use crate::clock::SharedClock;
use crate::runtime::{
    IdempotencyKey, NewPromptRequest, RequestKind, RequestKindPayload, SharedPromptQueue,
};
use crate::types::{Participant, Prompt};

use super::limits::{
    CONTRADICTION_SIMILARITY_THRESHOLD, DEDUP_SIMILARITY_THRESHOLD, LIBRARIAN_BATCH_LIMIT,
    LIBRARIAN_POLL_SECS, MAX_MEMORIES_PER_AGENT, MAX_SIMILAR_PAIRS_PER_AGENT,
    VALIDATION_DECAY_SECS,
};
use super::store::{
    MemoryMutation, MutationSource, PairCandidate, SharedMemoryStore, ValidationSource,
};

/// Summary of a single sweep's work — exposed for tests and observability.
#[derive(Debug, Default, Clone)]
pub struct LibrarianSweepReport {
    pub deduped: usize,
    pub demoted: usize,
    pub evicted: usize,
    pub contradictions_flagged: usize,
}

/// Run one mechanical sweep for a single agent.
///
/// Returns a report of what happened. Idempotent and pure: each
/// sub-operation goes through the store's transactional path, so a
/// partial failure leaves a consistent view (the run that retries
/// continues from the state on disk).
pub async fn run_librarian_sweep(
    store: &dyn super::store::MemoryStore,
    agent: AgentId,
    now: DateTime<Utc>,
) -> Result<LibrarianSweepReport, super::store::MemoryStoreError> {
    let mut report = LibrarianSweepReport::default();

    // 1. Dedup — pairs above similarity threshold merge.
    let dedup_pairs = store
        .similar_pairs(
            agent,
            DEDUP_SIMILARITY_THRESHOLD,
            MAX_SIMILAR_PAIRS_PER_AGENT,
        )
        .await?;
    let mut forgotten: std::collections::HashSet<crate::memory::MemoryId> =
        std::collections::HashSet::new();
    for pair in dedup_pairs {
        if forgotten.contains(&pair.a.id) || forgotten.contains(&pair.b.id) {
            continue;
        }
        let (keep, drop_) = pick_dedup_winner(&pair);
        // A duplicate from a different session is an independent re-write
        // signal — record_validation handles both the journal write and
        // the state promotion atomically.
        if keep.source_turn_id != drop_.source_turn_id {
            store
                .record_validation(
                    agent,
                    keep.id,
                    ValidationSource::CrossSessionRewrite,
                    Some("librarian dedup match"),
                )
                .await?;
        }
        // Forget the loser via the operator-override path so a pinned row
        // (rare but possible) survives. Pinned rows skip the merge.
        if drop_.pinned {
            continue;
        }
        store
            .apply(MemoryMutation::Forget {
                agent,
                target: drop_.id,
                source: MutationSource::Librarian,
                operator_override: false,
            })
            .await?;
        forgotten.insert(drop_.id);
        report.deduped += 1;
    }

    // 2. Decay — bump down stale Validated rows.
    let cutoff = now
        - chrono::Duration::seconds(
            i64::try_from(VALIDATION_DECAY_SECS).expect("invariant: decay secs fits in i64"),
        );
    report.demoted = store.decay_validated(agent, cutoff).await?;

    // 3. Eviction — bring per-agent count under quota.
    let evicted = store.evict_overflow(agent, MAX_MEMORIES_PER_AGENT).await?;
    report.evicted = evicted.len();

    // 4. Contradiction detection — pairs with high similarity but
    // opposing textual cues.
    let contradiction_pairs = store
        .similar_pairs(
            agent,
            CONTRADICTION_SIMILARITY_THRESHOLD,
            MAX_SIMILAR_PAIRS_PER_AGENT,
        )
        .await?;
    for pair in contradiction_pairs {
        if forgotten.contains(&pair.a.id) || forgotten.contains(&pair.b.id) {
            continue;
        }
        if pair.similarity >= DEDUP_SIMILARITY_THRESHOLD {
            // Duplicates handled in step 1.
            continue;
        }
        if !contradicts(pair.a.content.as_str(), pair.b.content.as_str()) {
            continue;
        }
        let reason = format!(
            "high embedding similarity ({:.3}) with opposing textual cues",
            pair.similarity
        );
        store
            .record_contradiction(agent, pair.a.id, pair.b.id, &reason)
            .await?;
        report.contradictions_flagged += 1;
    }

    Ok(report)
}

/// Pick the winner of a dedup pair: higher state priority wins, ties
/// broken by older `created_at`, then by smaller `id` for determinism.
fn pick_dedup_winner(pair: &PairCandidate) -> (&super::store::MemoryRow, &super::store::MemoryRow) {
    let prio_a = pair.a.state.priority();
    let prio_b = pair.b.state.priority();
    let a_wins = match prio_a.cmp(&prio_b) {
        std::cmp::Ordering::Greater => true,
        std::cmp::Ordering::Less => false,
        std::cmp::Ordering::Equal => match pair.a.created_at.cmp(&pair.b.created_at) {
            std::cmp::Ordering::Less => true,
            std::cmp::Ordering::Greater => false,
            std::cmp::Ordering::Equal => pair.a.id.as_uuid() < pair.b.id.as_uuid(),
        },
    };
    if a_wins {
        (&pair.a, &pair.b)
    } else {
        (&pair.b, &pair.a)
    }
}

/// Heuristic: does the textual content of `a` and `b` look like opposing
/// claims? Cheap pattern match for negation tokens; deliberately noisy on
/// the generous side — false positives become low-priority resolution
/// jobs, false negatives leave a real contradiction unflagged forever.
fn contradicts(a: &str, b: &str) -> bool {
    let a_neg = has_negation(a);
    let b_neg = has_negation(b);
    // Opposing if exactly one side carries a negation token; both having
    // (or both lacking) negation reads as the same direction.
    a_neg ^ b_neg
}

fn has_negation(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    let tokens = [
        "not ",
        "n't ",
        "never ",
        " no ",
        " avoid ",
        " forbid ",
        "do not",
        "don't",
        "shouldn't",
        "won't",
    ];
    tokens.iter().any(|t| lower.contains(t))
}

/// Background scheduler that runs the librarian sweep per agent on a
/// fixed cadence and enqueues resolution jobs for unresolved
/// contradictions (Phase 7).
pub struct LibrarianScheduler {
    shutdown: DropGuard,
    handle: JoinHandle<()>,
}

impl std::fmt::Debug for LibrarianScheduler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LibrarianScheduler").finish_non_exhaustive()
    }
}

impl LibrarianScheduler {
    /// Spawn with the production cadence. The internal cancel token reacts
    /// to either the supplied parent token or a drop of the handle.
    #[must_use]
    pub fn spawn(
        pool: PgPool,
        store: SharedMemoryStore,
        queue: SharedPromptQueue,
        clock: SharedClock,
        parent: CancellationToken,
    ) -> Self {
        Self::spawn_with_cadence(
            pool,
            store,
            queue,
            clock,
            Duration::from_secs(LIBRARIAN_POLL_SECS),
            Some(parent),
        )
    }

    #[must_use]
    pub fn spawn_with_cadence(
        pool: PgPool,
        store: SharedMemoryStore,
        queue: SharedPromptQueue,
        clock: SharedClock,
        poll_interval: Duration,
        parent: Option<CancellationToken>,
    ) -> Self {
        let owned = CancellationToken::new();
        let local_for_loop = owned.clone();
        let inner = SchedulerLoop {
            pool,
            store,
            queue,
            clock,
            poll_interval,
            batch_limit: LIBRARIAN_BATCH_LIMIT,
        };
        let handle = tokio::spawn(async move { inner.run(local_for_loop, parent).await });
        Self {
            shutdown: owned.drop_guard(),
            handle,
        }
    }

    pub async fn shutdown(self) {
        drop(self.shutdown);
        if let Err(e) = self.handle.await {
            warn!(error = %e, "librarian_scheduler.join.error");
        }
    }
}

#[derive(Debug)]
struct SchedulerLoop {
    pool: PgPool,
    store: SharedMemoryStore,
    queue: SharedPromptQueue,
    clock: SharedClock,
    poll_interval: Duration,
    batch_limit: usize,
}

impl SchedulerLoop {
    async fn run(self, local: CancellationToken, parent: Option<CancellationToken>) {
        loop {
            tokio::select! {
                biased;
                () = local.cancelled() => return,
                () = parent_cancelled(parent.as_ref()) => return,
                () = tokio::time::sleep(self.poll_interval) => {},
            }
            if let Err(e) = self.tick().await {
                warn!(error = %e, "librarian_scheduler.tick.error");
            }
        }
    }

    async fn tick(&self) -> Result<(), super::store::MemoryStoreError> {
        let now: DateTime<Utc> = self.clock.now_wall().into();
        let agents = self.list_agents().await?;

        for agent in agents.into_iter().take(self.batch_limit) {
            // Sweep first so any new contradictions become eligible for
            // enqueue in the same tick.
            match super::librarian::run_librarian_sweep(self.store.as_ref(), agent, now).await {
                Ok(report) => {
                    info!(
                        relay.agent.id = %agent,
                        relay.librarian.deduped = report.deduped,
                        relay.librarian.demoted = report.demoted,
                        relay.librarian.evicted = report.evicted,
                        relay.librarian.contradictions = report.contradictions_flagged,
                        "librarian.sweep.ok",
                    );
                }
                Err(e) => {
                    warn!(error = %e, relay.agent.id = %agent, "librarian.sweep.error");
                    continue;
                }
            }

            if let Err(e) = self.enqueue_resolutions(agent).await {
                warn!(error = %e, relay.agent.id = %agent, "librarian.enqueue.error");
            }
        }
        Ok(())
    }

    async fn list_agents(&self) -> Result<Vec<AgentId>, super::store::MemoryStoreError> {
        let limit = i64::try_from(self.batch_limit).expect("invariant: batch limit fits in i64");
        let rows: Vec<(AgentId,)> =
            sqlx::query_as("SELECT id FROM agents ORDER BY created_at LIMIT $1")
                .bind(limit)
                .fetch_all(&self.pool)
                .await?;
        Ok(rows.into_iter().map(|(id,)| id).collect())
    }

    async fn enqueue_resolutions(
        &self,
        agent: AgentId,
    ) -> Result<(), super::store::MemoryStoreError> {
        let unresolved = self.store.unresolved_contradictions(agent).await?;
        for ev in unresolved {
            // Idempotency: skip if a resolution row is already in flight
            // for this contradiction id. The queue's enqueue path already
            // dedups on idempotency_key, so we just hand it a key derived
            // from `ev.id`.
            let key = IdempotencyKey::try_from(format!("resolve-{}", ev.id))
                .expect("invariant: contradiction key fits cap");
            let viewer = Participant::agent(agent);
            let content = Prompt::try_from("(resolution)")
                .expect("invariant: resolution placeholder is valid");
            let req = NewPromptRequest {
                session: None,
                sender: viewer,
                receiver_agent_id: agent,
                parent_session: None,
                content,
                idempotency_key: key,
                kind: RequestKind::Resolution,
                kind_payload: RequestKindPayload::Resolution {
                    contradiction_event_id: ev.id,
                },
            };
            match self.queue.enqueue(req).await {
                Ok(outcome) => {
                    debug!(
                        relay.agent.id = %agent,
                        relay.contradiction.id = %ev.id,
                        relay.request.id = %outcome.request_id(),
                        "librarian.enqueue.resolution",
                    );
                }
                Err(e) => {
                    warn!(error = %e, "librarian.enqueue.resolution.error");
                }
            }
        }
        Ok(())
    }
}

async fn parent_cancelled(parent: Option<&CancellationToken>) {
    match parent {
        Some(token) => token.cancelled().await,
        None => std::future::pending::<()>().await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn negation_detection() {
        assert!(has_negation("don't ship on Friday"));
        assert!(has_negation("never push to main"));
        assert!(!has_negation("ship on Friday"));
        assert!(!has_negation("push to main"));
    }

    #[test]
    fn contradicts_only_when_one_side_negates() {
        assert!(contradicts("ship on Friday", "don't ship on Friday"));
        assert!(!contradicts("ship on Friday", "ship on Friday"));
        assert!(!contradicts("never ship on Friday", "don't ship on Friday"));
    }
}
