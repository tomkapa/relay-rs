//! Postgres-backed [`PromptQueue`] + [`LeaseManager`].
//!
//! Backs the trait surface with four tables: `prompt_requests`, `session_leases`,
//! `session_turn_seq`, `sessions`. The agent loop, worker pool, hooks, and HTTP
//! handlers depend only on the traits, so this lives entirely behind that seam.
//!
//! All wall-clock values come from the injected [`SharedClock`] — never `NOW()` in
//! app SQL — so a `TestClock`-driven test sees lease-expiry boundaries firing on
//! cue (CLAUDE.md §11). Status enums and ids cross the SQL boundary via the
//! `sqlx::Type` impls in [`super::types`]; no hand-rolled string matching survives
//! here.

use std::fmt;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::PgPool;

use crate::clock::SharedClock;
use crate::session::SessionId;
use crate::types::Prompt;

use super::error::PromptError;
use super::limits::{MAX_ATTEMPTS, MAX_PENDING_PER_SESSION};
use super::queue::{
    ClaimReceipt, ClaimedPrompt, ClaimedSession, EnqueueOutcome, LeaseManager, LeaseTiming,
    LeaseToken, NewPromptRequest, PromptQueue, RequestStatusView,
};
use super::types::{FailureReason, PromptRequestId, RequestStatus, TurnSeq, WorkerId};

/// Postgres-backed queue + lease manager.
///
/// One type implements both traits because the claim-and-drain critical section
/// needs a single transaction across `prompt_requests` and `session_leases`.
pub struct PgPromptQueue {
    pool: PgPool,
    clock: SharedClock,
    timing: LeaseTiming,
    pending_cap: u32,
    max_attempts: u32,
}

impl PgPromptQueue {
    /// Construct with default caps from `runtime::limits`.
    #[must_use]
    pub fn new(pool: PgPool, clock: SharedClock) -> Self {
        Self::with_caps(
            pool,
            clock,
            LeaseTiming::default_const(),
            MAX_PENDING_PER_SESSION,
            MAX_ATTEMPTS,
        )
    }

    #[must_use]
    pub fn with_caps(
        pool: PgPool,
        clock: SharedClock,
        timing: LeaseTiming,
        pending_cap: u32,
        max_attempts: u32,
    ) -> Self {
        Self {
            pool,
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

    fn now(&self) -> DateTime<Utc> {
        DateTime::<Utc>::from(self.clock.now_wall())
    }

    fn deadline(&self, now: DateTime<Utc>) -> DateTime<Utc> {
        now + chrono::Duration::from_std(self.timing.ttl())
            .expect("invariant: lease ttl fits in chrono::Duration")
    }

    /// Reset orphan rows — sessions whose lease expired before `now` are released, and
    /// any request stuck in `processing` for that session either returns to `pending`
    /// or, if it has already exhausted [`Self::max_attempts`], is parked as `failed`
    /// with `reason = poison`.
    async fn reset_orphans(&self, now: DateTime<Utc>) -> Result<(), PromptError> {
        let mut tx = self.pool.begin().await?;

        // Delete every expired lease in one shot, returning the (session, turn_seq)
        // pairs we need to fix up.
        let expired: Vec<(SessionId, TurnSeq)> = sqlx::query_as(
            "DELETE FROM session_leases
             WHERE leased_until <= $1
             RETURNING session_id, turn_seq",
        )
        .bind(now)
        .fetch_all(&mut *tx)
        .await?;

        if !expired.is_empty() {
            let max_attempts =
                i32::try_from(self.max_attempts).expect("invariant: max_attempts fits in i32");
            for (sid, stale_seq) in expired {
                // Combined update — for every processing row at the stale seq, either
                // park it as `failed` (attempts cap reached) or send it back to
                // `pending`. The failure reason binds typed via `FailureReason`'s
                // sqlx Encode (JSON-on-TEXT) so the Display form cannot drift into
                // storage by accident.
                sqlx::query(
                    "UPDATE prompt_requests
                     SET status = CASE WHEN attempts >= $1 THEN $2 ELSE $3 END,
                         failure_reason = CASE WHEN attempts >= $1 THEN $4 ELSE NULL END,
                         updated_at = $5
                     WHERE session_id = $6
                       AND status = $7
                       AND turn_seq = $8",
                )
                .bind(max_attempts)
                .bind(RequestStatus::Failed)
                .bind(RequestStatus::Pending)
                .bind(FailureReason::Poison)
                .bind(now)
                .bind(sid)
                .bind(RequestStatus::Processing)
                .bind(stale_seq)
                .execute(&mut *tx)
                .await?;
            }
        }

        tx.commit().await?;
        Ok(())
    }
}

impl fmt::Debug for PgPromptQueue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PgPromptQueue")
            .field("pending_cap", &self.pending_cap)
            .field("max_attempts", &self.max_attempts)
            .field("lease_ttl", &self.timing.ttl())
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl PromptQueue for PgPromptQueue {
    async fn enqueue(&self, req: NewPromptRequest) -> Result<EnqueueOutcome, PromptError> {
        let now = self.now();
        let mut tx = self.pool.begin().await?;

        // Idempotent path — return the existing row's id and status if the key is
        // already known. Lock the row to keep concurrent enqueues consistent.
        let existing: Option<(PromptRequestId, RequestStatus)> = sqlx::query_as(
            "SELECT id, status FROM prompt_requests
             WHERE idempotency_key = $1
             FOR UPDATE",
        )
        .bind(req.idempotency_key.as_str())
        .fetch_optional(&mut *tx)
        .await?;

        if let Some((request_id, status)) = existing {
            tx.commit().await?;
            return Ok(EnqueueOutcome::Existing { request_id, status });
        }

        // Pending-cap check. Counted inside the same tx as the insert so a racing
        // enqueue cannot push us past the cap.
        let pending_cap_i64 = i64::from(self.pending_cap);
        let (pending_count,): (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM prompt_requests
             WHERE session_id = $1 AND status = $2",
        )
        .bind(req.session)
        .bind(RequestStatus::Pending)
        .fetch_one(&mut *tx)
        .await?;
        if pending_count >= pending_cap_i64 {
            return Err(PromptError::PendingCapExceeded {
                session: req.session,
                max: self.pending_cap,
            });
        }

        let request_id = PromptRequestId::new();
        sqlx::query(
            "INSERT INTO prompt_requests
                 (id, session_id, content, idempotency_key, status,
                  attempts, turn_seq, cancellation_requested, failure_reason,
                  created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, 0, 0, FALSE, NULL, $6, $6)",
        )
        .bind(request_id)
        .bind(req.session)
        .bind(req.content.as_str())
        .bind(req.idempotency_key.as_str())
        .bind(RequestStatus::Pending)
        .bind(now)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;

        Ok(EnqueueOutcome::Inserted {
            request_id,
            status: RequestStatus::Pending,
        })
    }

    async fn claim_next_session(
        &self,
        worker: WorkerId,
    ) -> Result<Option<ClaimedSession>, PromptError> {
        let now = self.now();
        let deadline = self.deadline(now);

        // Reset orphans in its own transaction first so the candidate scan sees an
        // up-to-date world. Cheap when there's nothing to reset.
        self.reset_orphans(now).await?;

        let mut tx = self.pool.begin().await?;

        // Find the oldest pending request whose session has no live lease. The
        // partial index `prompt_requests_pending_idx (session_id, created_at) WHERE
        // status = 'pending'` is the relevant access path.
        let candidate: Option<(SessionId,)> = sqlx::query_as(
            "SELECT pr.session_id
             FROM prompt_requests pr
             WHERE pr.status = $1
               AND NOT EXISTS (
                   SELECT 1 FROM session_leases sl
                   WHERE sl.session_id = pr.session_id
                     AND sl.leased_until > $2
               )
             ORDER BY pr.created_at ASC
             LIMIT 1",
        )
        .bind(RequestStatus::Pending)
        .bind(now)
        .fetch_optional(&mut *tx)
        .await?;

        let Some((session,)) = candidate else {
            tx.commit().await?;
            return Ok(None);
        };

        // Bump the per-session monotonic counter. The seq table uses INSERT ... ON
        // CONFLICT to stay race-free.
        let (next_seq,): (TurnSeq,) = sqlx::query_as(
            "INSERT INTO session_turn_seq (session_id, next_seq)
             VALUES ($1, 1)
             ON CONFLICT (session_id) DO UPDATE
                 SET next_seq = session_turn_seq.next_seq + 1
             RETURNING next_seq",
        )
        .bind(session)
        .fetch_one(&mut *tx)
        .await?;

        // Try to take the lease. If another worker beat us to it (their lease is
        // still live), the WHERE clause on the ON CONFLICT branch fails and the
        // statement returns 0 rows — race-loss path.
        let lease: Option<(SessionId,)> = sqlx::query_as(
            "INSERT INTO session_leases (session_id, worker_id, turn_seq, leased_until)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (session_id) DO UPDATE
                 SET worker_id = EXCLUDED.worker_id,
                     turn_seq = EXCLUDED.turn_seq,
                     leased_until = EXCLUDED.leased_until
                 WHERE session_leases.leased_until <= $5
             RETURNING session_id",
        )
        .bind(session)
        .bind(worker)
        .bind(next_seq)
        .bind(deadline)
        .bind(now)
        .fetch_optional(&mut *tx)
        .await?;

        if lease.is_none() {
            // Race-lost — another worker holds the live lease for this session.
            tx.commit().await?;
            return Ok(None);
        }

        // Drain pending rows for the session: flip them to processing, stamp the
        // turn_seq, bump attempts.
        let drained: Vec<(PromptRequestId, String)> = sqlx::query_as(
            "UPDATE prompt_requests
             SET status = $1,
                 turn_seq = $2,
                 attempts = attempts + 1,
                 updated_at = $3
             WHERE session_id = $4 AND status = $5
             RETURNING id, content",
        )
        .bind(RequestStatus::Processing)
        .bind(next_seq)
        .bind(now)
        .bind(session)
        .bind(RequestStatus::Pending)
        .fetch_all(&mut *tx)
        .await?;

        tx.commit().await?;

        if drained.is_empty() {
            // All pending rows vanished between the candidate scan and the drain
            // (e.g. cancellation flipped them) — release the lease and report no work.
            let token = LeaseToken::build(session, worker, next_seq);
            let _ = self.release(&token).await;
            return Ok(None);
        }

        let mut prompts = Vec::with_capacity(drained.len());
        for (request_id, content) in drained {
            let parsed = Prompt::try_from(content)?;
            prompts.push(ClaimedPrompt {
                request_id,
                content: parsed,
            });
        }

        let lease = LeaseToken::build(session, worker, next_seq);

        Ok(Some(ClaimedSession {
            session,
            prompts,
            lease,
        }))
    }

    async fn mark_done(&self, receipt: &ClaimReceipt) -> Result<(), PromptError> {
        finalise(self, receipt, Finalisation::Done).await
    }

    async fn mark_failed(
        &self,
        receipt: &ClaimReceipt,
        reason: FailureReason,
    ) -> Result<(), PromptError> {
        finalise(self, receipt, Finalisation::Failed(reason)).await
    }

    async fn request_cancellation(&self, id: PromptRequestId) -> Result<(), PromptError> {
        let now = self.now();
        let res = sqlx::query(
            "UPDATE prompt_requests
             SET cancellation_requested = TRUE, updated_at = $1
             WHERE id = $2",
        )
        .bind(now)
        .bind(id)
        .execute(&self.pool)
        .await?;
        if res.rows_affected() == 0 {
            return Err(PromptError::RequestNotFound(id));
        }
        Ok(())
    }

    async fn status(&self, id: PromptRequestId) -> Result<RequestStatusView, PromptError> {
        let row: Option<(
            PromptRequestId,
            SessionId,
            RequestStatus,
            bool,
            Option<FailureReason>,
        )> = sqlx::query_as(
            "SELECT id, session_id, status, cancellation_requested, failure_reason
             FROM prompt_requests
             WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;

        let Some((request_id, session, status, cancellation_requested, failure_reason)) = row
        else {
            return Err(PromptError::RequestNotFound(id));
        };
        Ok(RequestStatusView {
            request_id,
            session,
            status,
            cancellation_requested,
            failure_reason,
        })
    }
}

#[async_trait]
impl LeaseManager for PgPromptQueue {
    async fn heartbeat(&self, lease: &LeaseToken) -> Result<(), PromptError> {
        let now = self.now();
        let deadline = self.deadline(now);
        let res = sqlx::query(
            "UPDATE session_leases
             SET leased_until = $1
             WHERE session_id = $2 AND worker_id = $3 AND turn_seq = $4",
        )
        .bind(deadline)
        .bind(lease.session())
        .bind(lease.worker())
        .bind(lease.turn_seq())
        .execute(&self.pool)
        .await?;
        if res.rows_affected() == 0 {
            return Err(PromptError::LeaseStale {
                session: lease.session(),
            });
        }
        Ok(())
    }

    async fn release(&self, lease: &LeaseToken) -> Result<(), PromptError> {
        // Silent no-op if the lease has already moved on — the row count is not
        // checked, since release racing with orphan reclamation is benign.
        sqlx::query(
            "DELETE FROM session_leases
             WHERE session_id = $1 AND worker_id = $2 AND turn_seq = $3",
        )
        .bind(lease.session())
        .bind(lease.worker())
        .bind(lease.turn_seq())
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

/// Outcome that drives [`finalise`]. Carries the (status, reason) pair as a
/// single value so callers cannot pass an inconsistent combination — the only
/// way to land in `failed` is to provide a [`FailureReason`].
#[derive(Debug)]
enum Finalisation {
    Done,
    Failed(FailureReason),
}

impl Finalisation {
    const fn status(&self) -> RequestStatus {
        match self {
            Self::Done => RequestStatus::Done,
            Self::Failed(_) => RequestStatus::Failed,
        }
    }

    fn reason(self) -> Option<FailureReason> {
        match self {
            Self::Done => None,
            Self::Failed(r) => Some(r),
        }
    }
}

/// Shared body of [`PgPromptQueue::mark_done`] / [`PgPromptQueue::mark_failed`]. Both
/// (a) verify the lease is still ours and (b) update every receipt id atomically.
async fn finalise(
    queue: &PgPromptQueue,
    receipt: &ClaimReceipt,
    outcome: Finalisation,
) -> Result<(), PromptError> {
    let now = queue.now();
    let lease = receipt.lease();
    let new_status = outcome.status();
    let failure_reason = outcome.reason();

    let mut tx = queue.pool.begin().await?;

    let (lease_ok,): (bool,) = sqlx::query_as(
        "SELECT EXISTS(
            SELECT 1 FROM session_leases
            WHERE session_id = $1 AND worker_id = $2 AND turn_seq = $3
         )",
    )
    .bind(lease.session())
    .bind(lease.worker())
    .bind(lease.turn_seq())
    .fetch_one(&mut *tx)
    .await?;

    if !lease_ok {
        return Err(PromptError::LeaseStale {
            session: lease.session(),
        });
    }

    sqlx::query(
        "UPDATE prompt_requests
         SET status = $1,
             failure_reason = $2,
             updated_at = $3
         WHERE id = ANY($4) AND session_id = $5 AND turn_seq = $6",
    )
    .bind(new_status)
    .bind(failure_reason)
    .bind(now)
    .bind(receipt.ids())
    .bind(lease.session())
    .bind(lease.turn_seq())
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(())
}
