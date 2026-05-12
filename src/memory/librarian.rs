//! Librarian — mechanical sweep + resolution-job enqueue
//! (doc/memory.md §1.8).
//!
//! The mechanical sweep ([`run_librarian_sweep`]) runs no LLM:
//!
//! 1. *Dedup* — pairs above [`DEDUP_SIMILARITY_THRESHOLD`] merge into the
//!    higher-state, older-provenance copy. The other side is forgotten;
//!    a `cross_session_rewrite` validation event lands on the survivor.
//! 2. *Decay* — [`MemoryStore::decay_validated`] demotes Validated rows
//!    whose `last_validated_at` is older than [`VALIDATION_DECAY`].
//! 3. *Eviction* — when over [`MAX_MEMORIES_PER_AGENT`], the lowest-scored
//!    non-pinned rows are forgotten.
//! 4. *Contradiction detection* — pairs at
//!    [`CONTRADICTION_SIMILARITY_THRESHOLD`] with opposing textual
//!    signals get a `contradiction_events` row.
//!
//! [`LibrarianScheduler`] runs the sweep periodically per agent and, for
//! each unresolved contradiction event, enqueues a `RequestKind::Resolution`
//! job that the worker pool dispatches to the agent's resolution path.

use std::fmt::Write as _;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::agents::AgentId;
use crate::clock::SharedClock;
use crate::runtime::{
    IdempotencyKey, NewPromptRequest, RequestKind, RequestKindPayload, SharedPromptQueue,
};
use crate::tools::truncate_from_start;
use crate::types::{PROMPT_MAX_BYTES, Participant, Prompt};

use super::limits::{
    CONTRADICTION_SIMILARITY_THRESHOLD, DEDUP_SIMILARITY_THRESHOLD, LIBRARIAN_BATCH_LIMIT,
    LIBRARIAN_POLL_SECS, MAX_MEMORIES_PER_AGENT, MAX_SIMILAR_PAIRS_PER_AGENT, VALIDATION_DECAY,
};
use super::scheduled_task::ScheduledTask;
use super::store::{
    ContradictionEventRow, MemoryMutation, MemoryRow, MutationSource, PairCandidate,
    SharedMemoryStore, ValidationSource,
};
use super::types::MemoryId;

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
        // Pinned losers survive the merge — the Librarian source never
        // bypasses pin protection, but skipping early avoids a wasted
        // `PinnedImmutable` error and is the spec'd behaviour.
        if drop_.pinned {
            continue;
        }
        store
            .apply(MemoryMutation::Forget {
                agent,
                target: drop_.id,
                source: MutationSource::Librarian,
            })
            .await?;
        forgotten.insert(drop_.id);
        report.deduped += 1;
    }

    // 2. Decay — bump down stale Validated rows.
    let cutoff = now - VALIDATION_DECAY;
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
/// fixed cadence and enqueues resolution jobs for unresolved contradictions.
#[derive(Debug)]
pub struct LibrarianScheduler {
    task: ScheduledTask,
}

impl LibrarianScheduler {
    /// Spawn with the production cadence. The parent token wires shutdown
    /// into the main runtime Ctrl+C signal.
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
        let inner = Arc::new(SchedulerInner {
            pool,
            store,
            queue,
            clock,
            batch_limit: LIBRARIAN_BATCH_LIMIT,
        });
        let task = ScheduledTask::spawn("librarian_scheduler", poll_interval, parent, move || {
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
    store: SharedMemoryStore,
    queue: SharedPromptQueue,
    clock: SharedClock,
    batch_limit: usize,
}

impl SchedulerInner {
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
            let (row_a, row_b) =
                tokio::try_join!(self.store.get(ev.memory_a), self.store.get(ev.memory_b))?;
            let content = build_resolution_prompt(&ev, row_a.as_ref(), row_b.as_ref());
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

/// Render the resolution turn's user prompt body: the two flagged
/// memories with their provenance, plus the librarian's reason.
///
/// Handles are fixed by convention — `M-1` for `memory_a`, `M-2` for
/// `memory_b` — matching the reserved-handle binding the
/// [`MemorySectionLoader`](super::loader::MemorySectionLoader) produces
/// for the same `contradiction_event_id`. A memory that was forgotten
/// between detection and enqueue renders as `[no longer exists]`; the
/// model's natural reply is then a plain-text no-action close.
///
/// Oversized renders trim the body from the head while preserving the
/// fixed framing header, matching
/// [`super::reflection_scheduler::build_reflection_prompt`]'s shape.
fn build_resolution_prompt(
    ev: &ContradictionEventRow,
    a: Option<&MemoryRow>,
    b: Option<&MemoryRow>,
) -> Prompt {
    const HEADER: &str = "Two of your memories were flagged as contradicting. \
        Decide what to do.\n\n";
    const NOTICE: &str = "[earlier content trimmed to fit prompt cap]\n";

    let mut body = String::new();
    let _ = writeln!(body, "Reason flagged: {}", ev.reason);
    body.push('\n');
    render_pair_entry(&mut body, "Memory A (M-1)", ev.memory_a, a);
    body.push('\n');
    render_pair_entry(&mut body, "Memory B (M-2)", ev.memory_b, b);

    let cap = PROMPT_MAX_BYTES;
    let out = if HEADER.len() + body.len() <= cap {
        let mut s = String::with_capacity(HEADER.len() + body.len());
        s.push_str(HEADER);
        s.push_str(&body);
        s
    } else {
        let max_body = cap.saturating_sub(HEADER.len() + NOTICE.len());
        let trimmed = truncate_from_start(&body, max_body);
        let mut s = String::with_capacity(HEADER.len() + NOTICE.len() + trimmed.len());
        s.push_str(HEADER);
        s.push_str(NOTICE);
        s.push_str(trimmed);
        s
    };
    Prompt::try_from(out).expect("invariant: body trimmed to Prompt cap")
}

fn render_pair_entry(buf: &mut String, label: &str, id: MemoryId, row: Option<&MemoryRow>) {
    let _ = writeln!(buf, "{label}:");
    let _ = writeln!(buf, "  Id: {id}");
    let Some(r) = row else {
        let _ = writeln!(
            buf,
            "  [no longer exists — likely forgotten before resolution]"
        );
        return;
    };
    let _ = writeln!(buf, "  Content: {}", r.content.as_str());
    let _ = writeln!(
        buf,
        "  Kind: {kind}   State: {state}   Pinned: {pinned}",
        kind = r.kind.as_str(),
        state = r.state.as_str(),
        pinned = r.pinned,
    );
    let _ = writeln!(buf, "  Created: {}", r.created_at.to_rfc3339());
    let _ = writeln!(
        buf,
        "  Last validated: {}",
        r.last_validated_at.to_rfc3339()
    );
    let _ = writeln!(buf, "  Access count: {}", r.access_count);
    match r.source_turn_id {
        Some(turn) => {
            let _ = writeln!(buf, "  Source turn: {turn}");
        }
        None => {
            buf.push_str("  Source turn: (operator note or librarian merge)\n");
        }
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

    use super::super::store::ContradictionEventRow;
    use super::super::types::{MemoryContent, MemoryKind, MemoryState};
    use chrono::TimeZone;

    fn fixture_row(id: MemoryId, content: &str, kind: MemoryKind, state: MemoryState) -> MemoryRow {
        MemoryRow {
            id,
            agent_id: AgentId::new(),
            kind,
            content: MemoryContent::try_from(content).expect("valid"),
            state,
            pinned: false,
            source_turn_id: None,
            created_at: Utc.timestamp_opt(1_700_000_000, 0).unwrap(),
            last_validated_at: Utc.timestamp_opt(1_700_000_000, 0).unwrap(),
            last_accessed_at: Utc.timestamp_opt(1_700_000_000, 0).unwrap(),
            access_count: 3,
        }
    }

    fn fixture_event(a: MemoryId, b: MemoryId, reason: &str) -> ContradictionEventRow {
        ContradictionEventRow {
            id: crate::memory::ContradictionEventId::new(),
            agent_id: AgentId::new(),
            memory_a: a,
            memory_b: b,
            reason: reason.into(),
            created_at: Utc.timestamp_opt(1_700_000_000, 0).unwrap(),
            resolved_at: None,
            resolution_event_id: None,
            resolution_reason: None,
        }
    }

    #[test]
    fn resolution_prompt_renders_both_memories_with_provenance() {
        let a = MemoryId::new();
        let b = MemoryId::new();
        let row_a = fixture_row(
            a,
            "ship on Friday",
            MemoryKind::Procedure,
            MemoryState::Held,
        );
        let row_b = fixture_row(
            b,
            "don't ship on Friday",
            MemoryKind::Procedure,
            MemoryState::Tentative,
        );
        let ev = fixture_event(a, b, "high similarity with opposing cues");

        let prompt = build_resolution_prompt(&ev, Some(&row_a), Some(&row_b));
        let s = prompt.as_str();

        assert!(s.contains("Reason flagged: high similarity"));
        assert!(s.contains("Memory A (M-1):"));
        assert!(s.contains("Memory B (M-2):"));
        assert!(s.contains("ship on Friday"));
        assert!(s.contains("don't ship on Friday"));
        assert!(s.contains("Kind: procedure"));
        assert!(s.contains("State: held"));
        assert!(s.contains("State: tentative"));
        assert!(s.contains("Source turn: (operator note or librarian merge)"));
    }

    #[test]
    fn resolution_prompt_degrades_when_memory_missing() {
        // A memory forgotten between detection and the resolution turn
        // renders a placeholder line. The model's natural reply is then
        // a no-action close.
        let a = MemoryId::new();
        let b = MemoryId::new();
        let row_a = fixture_row(a, "still here", MemoryKind::Other, MemoryState::Held);
        let ev = fixture_event(a, b, "flagged");

        let prompt = build_resolution_prompt(&ev, Some(&row_a), None);
        let s = prompt.as_str();
        assert!(s.contains("Memory A (M-1):"));
        assert!(s.contains("Memory B (M-2):"));
        assert!(s.contains("still here"));
        assert!(
            s.contains("[no longer exists"),
            "missing memory placeholder rendered: {s}"
        );
    }
}
