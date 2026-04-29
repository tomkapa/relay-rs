//! Prompt queue and lease management.
//!
//! Two traits — [`PromptQueue`] and [`LeaseManager`] — partition the surface so a HTTP
//! handler only sees the producer side and the worker only sees the consumer/lease
//! side. The in-memory implementation lives in this file and is shared by both because
//! claim-and-drain mutates queue state and lease state in one critical section.

use std::collections::HashMap;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;

use crate::clock::SharedClock;
use crate::session::SessionId;
use crate::types::Prompt;

use super::error::{LeaseTimingError, PromptError};
use super::limits::{LEASE_HEARTBEAT_INTERVAL, LEASE_TTL, MAX_ATTEMPTS, MAX_PENDING_PER_SESSION};
use super::types::{
    Attempts, FailureReason, IdempotencyKey, PromptRequestId, RequestStatus, TurnSeq, WorkerId,
};

/// Co-validated lease timing.
///
/// Holds the lease TTL and the heartbeat cadence as one value so the queue and
/// the worker pool cannot drift apart at runtime: a queue's `lease_ttl` must
/// always exceed its worker's `heartbeat_interval`, otherwise the lease silently
/// dies between beats. Constructed via [`LeaseTiming::try_new`] which enforces
/// that invariant; both [`InMemoryPromptQueue::with_caps`] and
/// [`crate::runtime::WorkerConfig`] take a `LeaseTiming` so a single value seeds
/// both sides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeaseTiming {
    ttl: Duration,
    heartbeat_interval: Duration,
}

impl LeaseTiming {
    /// Default timing from `runtime::limits` — `LEASE_TTL` / `LEASE_HEARTBEAT_INTERVAL`.
    /// The const-asserted ratio (`heartbeat * 3 == ttl`) holds at compile time, so the
    /// validation in [`Self::try_new`] is unconditional here — `expect` is a named
    /// assertion per CLAUDE.md §6.
    #[must_use]
    pub fn default_const() -> Self {
        Self::try_new(LEASE_TTL, LEASE_HEARTBEAT_INTERVAL)
            .expect("invariant: default LEASE_TTL/LEASE_HEARTBEAT_INTERVAL satisfy try_new")
    }

    /// Validate that `heartbeat_interval` is strictly less than `ttl` (so a missed
    /// beat still leaves time to recover) and non-zero.
    pub fn try_new(ttl: Duration, heartbeat_interval: Duration) -> Result<Self, LeaseTimingError> {
        if heartbeat_interval.is_zero() {
            return Err(LeaseTimingError::IntervalZero);
        }
        if heartbeat_interval >= ttl {
            return Err(LeaseTimingError::IntervalNotUnderTtl {
                ttl,
                heartbeat_interval,
            });
        }
        Ok(Self {
            ttl,
            heartbeat_interval,
        })
    }

    #[must_use]
    pub const fn ttl(&self) -> Duration {
        self.ttl
    }

    #[must_use]
    pub const fn heartbeat_interval(&self) -> Duration {
        self.heartbeat_interval
    }
}

impl Default for LeaseTiming {
    fn default() -> Self {
        Self::default_const()
    }
}

/// Producer-side surface used by HTTP handlers.
#[async_trait]
pub trait PromptQueue: std::fmt::Debug + Send + Sync {
    async fn enqueue(&self, req: NewPromptRequest) -> Result<EnqueueOutcome, PromptError>;
    async fn claim_next_session(
        &self,
        worker: WorkerId,
    ) -> Result<Option<ClaimedSession>, PromptError>;
    /// Mark every request in `receipt` as `Done`. The receipt binds the lease and the
    /// claimed ids together — there is no API for passing a foreign id list, so the
    /// "every id belongs to this lease's session" invariant is enforced by
    /// construction (and asserted again at the impl boundary).
    async fn mark_done(&self, receipt: &ClaimReceipt) -> Result<(), PromptError>;
    /// As [`mark_done`](Self::mark_done) but parks the rows as `Failed` with `reason`.
    async fn mark_failed(
        &self,
        receipt: &ClaimReceipt,
        reason: FailureReason,
    ) -> Result<(), PromptError>;
    async fn request_cancellation(&self, id: PromptRequestId) -> Result<(), PromptError>;
    /// Status accessor used by the SSE and cancel handlers — never required to be live;
    /// a snapshot is sufficient.
    async fn status(&self, id: PromptRequestId) -> Result<RequestStatusView, PromptError>;
}

/// Lease-side surface — heartbeat and release. The token is opaque; only the impl
/// inspects its contents.
#[async_trait]
pub trait LeaseManager: std::fmt::Debug + Send + Sync {
    async fn heartbeat(&self, lease: &LeaseToken) -> Result<(), PromptError>;
    async fn release(&self, lease: &LeaseToken) -> Result<(), PromptError>;
}

/// Input to [`PromptQueue::enqueue`]. Server-side fields (`request_id`, `attempts`,
/// `turn_seq`) are minted by the queue, never carried in.
#[derive(Debug, Clone)]
pub struct NewPromptRequest {
    pub session: SessionId,
    pub content: Prompt,
    pub idempotency_key: IdempotencyKey,
}

/// Outcome of an enqueue. Idempotent retries return [`EnqueueOutcome::Existing`] so the
/// client gets the original request id back instead of a fresh one.
#[derive(Debug, Clone)]
pub enum EnqueueOutcome {
    /// A new row was inserted.
    Inserted {
        request_id: PromptRequestId,
        status: RequestStatus,
    },
    /// The (idempotency_key) was already present; the original request id is returned
    /// verbatim along with its current status.
    Existing {
        request_id: PromptRequestId,
        status: RequestStatus,
    },
}

impl EnqueueOutcome {
    #[must_use]
    pub const fn request_id(&self) -> PromptRequestId {
        match self {
            Self::Inserted { request_id, .. } | Self::Existing { request_id, .. } => *request_id,
        }
    }

    #[must_use]
    pub const fn status(&self) -> RequestStatus {
        match self {
            Self::Inserted { status, .. } | Self::Existing { status, .. } => *status,
        }
    }
}

/// Snapshot returned to a worker on a successful claim. The `prompts` are drained from
/// the queue under the same critical section as the lease so a second worker sees a
/// stable picture.
#[derive(Debug, Clone)]
pub struct ClaimedSession {
    pub session: SessionId,
    pub prompts: Vec<ClaimedPrompt>,
    pub lease: LeaseToken,
}

impl ClaimedSession {
    /// Materialise a [`ClaimReceipt`] binding this claim's lease to the ids it
    /// drained. The worker carries the receipt through `mark_done` / `mark_failed`
    /// so those calls cannot be passed ids from a different session by accident.
    #[must_use]
    pub fn receipt(&self) -> ClaimReceipt {
        ClaimReceipt {
            lease: self.lease.clone(),
            ids: self.prompts.iter().map(|p| p.request_id).collect(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ClaimedPrompt {
    pub request_id: PromptRequestId,
    pub content: Prompt,
}

/// Proof that a worker holds a lease *and* the ids it drained under that lease.
///
/// The only handle accepted by [`PromptQueue::mark_done`] /
/// [`PromptQueue::mark_failed`]. Constructed solely via
/// [`ClaimedSession::receipt`]; both fields are private so a caller cannot forge
/// a receipt that mixes ids from another session with this lease (CLAUDE.md §1:
/// parse, don't validate).
#[derive(Debug, Clone)]
pub struct ClaimReceipt {
    lease: LeaseToken,
    ids: Vec<PromptRequestId>,
}

impl ClaimReceipt {
    #[must_use]
    pub const fn lease(&self) -> &LeaseToken {
        &self.lease
    }

    #[must_use]
    pub fn ids(&self) -> &[PromptRequestId] {
        &self.ids
    }
}

/// Opaque proof of lease ownership. The worker carries this through every write so the
/// impl can fence stale operations with a `WHERE turn_seq = $token` style check.
#[derive(Debug, Clone)]
pub struct LeaseToken {
    session: SessionId,
    worker: WorkerId,
    turn_seq: TurnSeq,
}

impl LeaseToken {
    #[must_use]
    pub const fn session(&self) -> SessionId {
        self.session
    }
    #[must_use]
    pub const fn worker(&self) -> WorkerId {
        self.worker
    }
    #[must_use]
    pub const fn turn_seq(&self) -> TurnSeq {
        self.turn_seq
    }
}

/// Cheap read-side view returned by [`PromptQueue::status`].
#[derive(Debug, Clone)]
pub struct RequestStatusView {
    pub request_id: PromptRequestId,
    pub session: SessionId,
    pub status: RequestStatus,
    pub cancellation_requested: bool,
    pub failure_reason: Option<String>,
}

/// Reference-counted producer-side handle. The HTTP layer holds one and dispatches
/// dynamically through the trait.
pub type SharedPromptQueue = Arc<dyn PromptQueue>;

/// Reference-counted lease-side handle held by workers.
pub type SharedLeaseManager = Arc<dyn LeaseManager>;

// ============================================================================
// In-memory implementation
// ============================================================================

#[derive(Debug)]
struct PromptRow {
    session: SessionId,
    content: Prompt,
    status: RequestStatus,
    attempts: Attempts,
    turn_seq: TurnSeq,
    cancellation_requested: bool,
    failure_reason: Option<String>,
    /// Held for parity with the Pg row (queryable via the unique index).
    #[allow(dead_code)]
    idempotency_key: IdempotencyKey,
}

#[derive(Debug)]
struct LeaseRow {
    worker: WorkerId,
    turn_seq: TurnSeq,
    /// Wall-clock instant at which this lease expires. Updated on heartbeat; cleared
    /// on release.
    leased_until: std::time::Instant,
}

#[derive(Debug)]
struct State {
    requests: HashMap<PromptRequestId, PromptRow>,
    leases: HashMap<SessionId, LeaseRow>,
    /// Per-session FIFO of pending request ids. Drained on claim. Capped at
    /// [`MAX_PENDING_PER_SESSION`] entries.
    pending: HashMap<SessionId, VecDeque<PromptRequestId>>,
    /// Idempotency-key dedup map.
    idempotency: HashMap<IdempotencyKey, PromptRequestId>,
    /// Per-session turn-seq counter — the next value to hand out on claim.
    next_turn_seq: HashMap<SessionId, TurnSeq>,
}

impl State {
    fn empty() -> Self {
        Self {
            requests: HashMap::new(),
            leases: HashMap::new(),
            pending: HashMap::new(),
            idempotency: HashMap::new(),
            next_turn_seq: HashMap::new(),
        }
    }
}

/// Process-local prompt queue + lease manager. Suitable for the in-process binary; a
/// `Pg*` impl drops in behind the same traits when storage moves to Postgres.
#[derive(Debug)]
pub struct InMemoryPromptQueue {
    state: Mutex<State>,
    clock: SharedClock,
    timing: LeaseTiming,
    pending_cap: usize,
    max_attempts: u32,
}

impl InMemoryPromptQueue {
    #[must_use]
    pub fn new(clock: SharedClock) -> Self {
        Self::with_caps(
            clock,
            LeaseTiming::default_const(),
            MAX_PENDING_PER_SESSION,
            MAX_ATTEMPTS,
        )
    }

    #[must_use]
    pub fn with_caps(
        clock: SharedClock,
        timing: LeaseTiming,
        pending_cap: usize,
        max_attempts: u32,
    ) -> Self {
        Self {
            state: Mutex::new(State::empty()),
            clock,
            timing,
            pending_cap,
            max_attempts,
        }
    }

    /// Lease timing — used by the worker pool to keep its heartbeat cadence in sync
    /// with the queue's TTL.
    #[must_use]
    pub const fn lease_timing(&self) -> LeaseTiming {
        self.timing
    }

    /// Lock the inner state. The mutex is held only across synchronous critical
    /// sections (no `.await` while held), so a `std::sync::Mutex` is correct here per
    /// tokio's own guidance.
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .expect("invariant: queue state mutex never poisoned")
    }
}

impl State {
    fn next_seq_for(&mut self, session: SessionId) -> Result<TurnSeq, PromptError> {
        let cur = self.next_turn_seq.entry(session).or_insert(TurnSeq::ZERO);
        let next = cur.next()?;
        *cur = next;
        Ok(next)
    }
}

#[async_trait]
impl PromptQueue for InMemoryPromptQueue {
    async fn enqueue(&self, req: NewPromptRequest) -> Result<EnqueueOutcome, PromptError> {
        let mut guard = self.state();

        if let Some(existing) = guard.idempotency.get(&req.idempotency_key).copied() {
            // §6: idempotency table and request map must agree.
            let row = guard
                .requests
                .get(&existing)
                .expect("invariant: idempotency entry without backing request");
            return Ok(EnqueueOutcome::Existing {
                request_id: existing,
                status: row.status,
            });
        }

        let pending = guard.pending.entry(req.session).or_default();
        if pending.len() >= self.pending_cap {
            return Err(PromptError::PendingCapExceeded {
                session: req.session,
                max: self.pending_cap,
            });
        }

        let request_id = PromptRequestId::new();
        pending.push_back(request_id);

        guard
            .idempotency
            .insert(req.idempotency_key.clone(), request_id);
        let inserted = guard.requests.insert(
            request_id,
            PromptRow {
                session: req.session,
                content: req.content,
                status: RequestStatus::Pending,
                attempts: Attempts::ZERO,
                turn_seq: TurnSeq::ZERO,
                cancellation_requested: false,
                failure_reason: None,
                idempotency_key: req.idempotency_key,
            },
        );
        assert!(inserted.is_none(), "PromptRequestId collision");

        Ok(EnqueueOutcome::Inserted {
            request_id,
            status: RequestStatus::Pending,
        })
    }

    async fn claim_next_session(
        &self,
        worker: WorkerId,
    ) -> Result<Option<ClaimedSession>, PromptError> {
        let mut guard = self.state();
        let now = self.clock.now();

        // Reset orphan rows whose lease has expired before scanning for work.
        reset_orphans(&mut guard, now, self.max_attempts);

        // Find a session that has pending work AND no live lease.
        let candidate = guard.pending.iter().find_map(|(session, queue)| {
            if queue.is_empty() {
                return None;
            }
            let leased = guard
                .leases
                .get(session)
                .is_some_and(|l| l.leased_until > now);
            if leased { None } else { Some(*session) }
        });

        let Some(session) = candidate else {
            return Ok(None);
        };

        // Drain pending; assert at least one is present (we just checked).
        let drained: Vec<PromptRequestId> = guard
            .pending
            .get_mut(&session)
            .map(|q| q.drain(..).collect())
            .unwrap_or_default();
        assert!(
            !drained.is_empty(),
            "candidate session must have pending work"
        );

        let turn_seq = guard.next_seq_for(session)?;
        let leased_until = now + self.timing.ttl();
        guard.leases.insert(
            session,
            LeaseRow {
                worker,
                turn_seq,
                leased_until,
            },
        );

        let mut prompts = Vec::with_capacity(drained.len());
        for id in &drained {
            let row = guard
                .requests
                .get_mut(id)
                .expect("invariant: pending id must be present in requests");
            row.status = RequestStatus::Processing;
            row.attempts.increment();
            row.turn_seq = turn_seq;
            prompts.push(ClaimedPrompt {
                request_id: *id,
                content: row.content.clone(),
            });
        }

        Ok(Some(ClaimedSession {
            session,
            prompts,
            lease: LeaseToken {
                session,
                worker,
                turn_seq,
            },
        }))
    }

    async fn mark_done(&self, receipt: &ClaimReceipt) -> Result<(), PromptError> {
        let mut guard = self.state();
        let lease = receipt.lease();
        if !lease_matches(&guard, lease) {
            return Err(PromptError::LeaseStale {
                session: lease.session,
            });
        }
        for id in receipt.ids() {
            if let Some(row) = guard.requests.get_mut(id) {
                // §6 belt-and-braces — the receipt's construction already guarantees
                // every id was drained under this lease's session, but a row whose
                // session disagrees would mean the queue's own bookkeeping is wrong.
                assert_eq!(
                    row.session, lease.session,
                    "invariant: receipt id {id} belongs to lease's session"
                );
                row.status = RequestStatus::Done;
            }
        }
        Ok(())
    }

    async fn mark_failed(
        &self,
        receipt: &ClaimReceipt,
        reason: FailureReason,
    ) -> Result<(), PromptError> {
        let mut guard = self.state();
        let lease = receipt.lease();
        if !lease_matches(&guard, lease) {
            return Err(PromptError::LeaseStale {
                session: lease.session,
            });
        }
        let label = reason.to_string();
        for id in receipt.ids() {
            if let Some(row) = guard.requests.get_mut(id) {
                assert_eq!(
                    row.session, lease.session,
                    "invariant: receipt id {id} belongs to lease's session"
                );
                row.status = RequestStatus::Failed;
                row.failure_reason = Some(label.clone());
            }
        }
        Ok(())
    }

    async fn request_cancellation(&self, id: PromptRequestId) -> Result<(), PromptError> {
        let mut guard = self.state();
        let row = guard
            .requests
            .get_mut(&id)
            .ok_or(PromptError::RequestNotFound(id))?;
        row.cancellation_requested = true;
        Ok(())
    }

    async fn status(&self, id: PromptRequestId) -> Result<RequestStatusView, PromptError> {
        let guard = self.state();
        let row = guard
            .requests
            .get(&id)
            .ok_or(PromptError::RequestNotFound(id))?;
        Ok(RequestStatusView {
            request_id: id,
            session: row.session,
            status: row.status,
            cancellation_requested: row.cancellation_requested,
            failure_reason: row.failure_reason.clone(),
        })
    }
}

#[async_trait]
impl LeaseManager for InMemoryPromptQueue {
    async fn heartbeat(&self, lease: &LeaseToken) -> Result<(), PromptError> {
        let mut guard = self.state();
        let now = self.clock.now();
        let entry = guard
            .leases
            .get_mut(&lease.session)
            .ok_or(PromptError::LeaseStale {
                session: lease.session,
            })?;
        if entry.turn_seq != lease.turn_seq || entry.worker != lease.worker {
            return Err(PromptError::LeaseStale {
                session: lease.session,
            });
        }
        entry.leased_until = now + self.timing.ttl();
        Ok(())
    }

    async fn release(&self, lease: &LeaseToken) -> Result<(), PromptError> {
        let mut guard = self.state();
        if let Some(entry) = guard.leases.get(&lease.session)
            && entry.turn_seq == lease.turn_seq
            && entry.worker == lease.worker
        {
            guard.leases.remove(&lease.session);
        }
        Ok(())
    }
}

fn lease_matches(state: &State, lease: &LeaseToken) -> bool {
    state
        .leases
        .get(&lease.session)
        .is_some_and(|l| l.turn_seq == lease.turn_seq && l.worker == lease.worker)
}

/// Recover any session whose lease has expired: requests in `processing` for that
/// session but with a stale `turn_seq` are returned to `pending`. Once a row's attempt
/// count reaches `max_attempts` it is parked as `failed` with `reason = poison`.
fn reset_orphans(state: &mut State, now: std::time::Instant, max_attempts: u32) {
    let expired: Vec<SessionId> = state
        .leases
        .iter()
        .filter_map(|(s, l)| {
            if l.leased_until <= now {
                Some(*s)
            } else {
                None
            }
        })
        .collect();

    for session in expired {
        let stale_seq = state
            .leases
            .remove(&session)
            .map_or(TurnSeq::ZERO, |l| l.turn_seq);

        let orphan_ids: Vec<PromptRequestId> = state
            .requests
            .iter()
            .filter_map(|(id, row)| {
                if row.session == session
                    && row.status == RequestStatus::Processing
                    && row.turn_seq <= stale_seq
                {
                    Some(*id)
                } else {
                    None
                }
            })
            .collect();

        for id in orphan_ids {
            // Borrow-split — re-fetch under mutable guard.
            let row = state
                .requests
                .get_mut(&id)
                .expect("invariant: id collected above must still be present");
            if row.attempts.get() >= max_attempts {
                row.status = RequestStatus::Failed;
                row.failure_reason = Some(FailureReason::Poison.to_string());
                continue;
            }
            row.status = RequestStatus::Pending;
            state.pending.entry(session).or_default().push_back(id);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use super::*;
    use crate::clock::{SharedClock, TestClock};
    use crate::session::SessionId;
    use crate::types::Prompt;

    fn fresh() -> (Arc<InMemoryPromptQueue>, Arc<TestClock>) {
        let test_clock = Arc::new(TestClock::new());
        let clock: SharedClock = test_clock.clone();
        let q = Arc::new(InMemoryPromptQueue::new(clock));
        (q, test_clock)
    }

    fn req(session: SessionId, content: &str, key: &str) -> NewPromptRequest {
        NewPromptRequest {
            session,
            content: Prompt::try_from(content).expect("prompt"),
            idempotency_key: IdempotencyKey::try_from(key).expect("key"),
        }
    }

    #[tokio::test]
    async fn enqueue_is_idempotent_on_repeat_key() {
        let (q, _c) = fresh();
        let s = SessionId::new();
        let first = q.enqueue(req(s, "hi", "k1")).await.expect("first");
        let second = q.enqueue(req(s, "hi-again", "k1")).await.expect("second");
        assert_eq!(first.request_id(), second.request_id());
        assert!(matches!(second, EnqueueOutcome::Existing { .. }));
    }

    #[tokio::test]
    async fn enqueue_caps_pending_per_session() {
        let test_clock = Arc::new(TestClock::new());
        let clock: SharedClock = test_clock;
        let q =
            InMemoryPromptQueue::with_caps(clock, LeaseTiming::default_const(), 2, MAX_ATTEMPTS);
        let s = SessionId::new();
        q.enqueue(req(s, "a", "k1")).await.expect("ok1");
        q.enqueue(req(s, "b", "k2")).await.expect("ok2");
        let err = q.enqueue(req(s, "c", "k3")).await.expect_err("over cap");
        assert!(matches!(err, PromptError::PendingCapExceeded { .. }));
    }

    #[tokio::test]
    async fn claim_drains_all_pending_for_session() {
        let (q, _c) = fresh();
        let s = SessionId::new();
        let r1 = q.enqueue(req(s, "a", "k1")).await.expect("ok").request_id();
        let r2 = q.enqueue(req(s, "b", "k2")).await.expect("ok").request_id();
        let claimed = q
            .claim_next_session(WorkerId::new())
            .await
            .expect("claim")
            .expect("some");
        assert_eq!(claimed.prompts.len(), 2);
        let ids: Vec<_> = claimed.prompts.iter().map(|p| p.request_id).collect();
        assert!(ids.contains(&r1));
        assert!(ids.contains(&r2));
    }

    #[tokio::test]
    async fn second_claim_skips_leased_session() {
        let (q, _c) = fresh();
        let s = SessionId::new();
        let _ = q.enqueue(req(s, "a", "k1")).await.expect("ok");
        let _first = q
            .claim_next_session(WorkerId::new())
            .await
            .expect("claim")
            .expect("some");
        // No further work on this session, lease still live → second claim is None.
        let second = q.claim_next_session(WorkerId::new()).await.expect("claim2");
        assert!(second.is_none());
    }

    #[tokio::test]
    async fn lease_expiry_returns_orphan_to_pending() {
        let (q, clock) = fresh();
        let s = SessionId::new();
        let _ = q.enqueue(req(s, "a", "k1")).await.expect("ok");
        let _ = q
            .claim_next_session(WorkerId::new())
            .await
            .expect("claim")
            .expect("some");

        // Advance past lease TTL — the original worker is dead.
        clock.advance(LEASE_TTL + Duration::from_secs(1));

        let again = q
            .claim_next_session(WorkerId::new())
            .await
            .expect("reclaim")
            .expect("orphan recovered");
        assert_eq!(again.prompts.len(), 1);
    }

    #[tokio::test]
    async fn mark_done_with_stale_token_fails() {
        let (q, clock) = fresh();
        let s = SessionId::new();
        let r1 = q.enqueue(req(s, "a", "k1")).await.expect("ok").request_id();
        let claim1 = q
            .claim_next_session(WorkerId::new())
            .await
            .expect("c1")
            .expect("some");

        clock.advance(LEASE_TTL + Duration::from_secs(1));

        let _claim2 = q
            .claim_next_session(WorkerId::new())
            .await
            .expect("c2")
            .expect("some");

        // The original (zombie) worker tries to write — fenced off by turn_seq.
        let receipt = claim1.receipt();
        let err = q.mark_done(&receipt).await.expect_err("stale");
        assert!(matches!(err, PromptError::LeaseStale { .. }));
        let _ = r1;
    }

    #[tokio::test]
    async fn poisons_after_max_attempts_via_orphan_path() {
        let test_clock = Arc::new(TestClock::new());
        let clock: SharedClock = test_clock.clone();
        // Force MAX_ATTEMPTS = 2 to keep the test short.
        let q = InMemoryPromptQueue::with_caps(clock, LeaseTiming::default_const(), 8, 2);
        let s = SessionId::new();
        let r = q.enqueue(req(s, "a", "k1")).await.expect("ok").request_id();

        // Attempt 1: claim, then let lease expire (worker died mid-turn).
        let _ = q.claim_next_session(WorkerId::new()).await.expect("c1");
        test_clock.advance(LEASE_TTL + Duration::from_secs(1));

        // Attempt 2: claim again, let lease expire — now hits the cap.
        let _ = q
            .claim_next_session(WorkerId::new())
            .await
            .expect("c2")
            .expect("some");
        test_clock.advance(LEASE_TTL + Duration::from_secs(1));

        // A third claim resets orphans; the poisoned row is now `failed`.
        let _ = q.claim_next_session(WorkerId::new()).await.expect("c3");
        let view = q.status(r).await.expect("status");
        assert!(matches!(view.status, RequestStatus::Failed));
        assert!(view.failure_reason.is_some());
    }

    #[tokio::test]
    async fn heartbeat_extends_lease() {
        let (q, clock) = fresh();
        let s = SessionId::new();
        let _ = q.enqueue(req(s, "a", "k1")).await.expect("ok");
        let claim = q
            .claim_next_session(WorkerId::new())
            .await
            .expect("c")
            .expect("some");

        // Just under TTL — heartbeat — then advance again — still owned.
        clock.advance(LEASE_TTL.saturating_sub(Duration::from_secs(1)));
        q.heartbeat(&claim.lease).await.expect("heartbeat");
        clock.advance(LEASE_TTL.saturating_sub(Duration::from_secs(1)));
        q.heartbeat(&claim.lease).await.expect("heartbeat2");

        // mark_done still works — proof the lease is still ours.
        q.mark_done(&claim.receipt()).await.expect("done");
    }

    #[tokio::test]
    async fn release_clears_lease_so_others_can_claim() {
        let (q, _c) = fresh();
        let s = SessionId::new();
        let _ = q.enqueue(req(s, "a", "k1")).await.expect("ok");
        let claim = q
            .claim_next_session(WorkerId::new())
            .await
            .expect("c")
            .expect("some");
        q.mark_done(&claim.receipt()).await.expect("done");
        q.release(&claim.lease).await.expect("release");

        // Re-enqueue + claim should work without waiting.
        let s2 = SessionId::new();
        let _ = q.enqueue(req(s2, "b", "k2")).await.expect("ok");
        let again = q.claim_next_session(WorkerId::new()).await.expect("c2");
        assert!(again.is_some());
    }
}
