//! Trait-contract tests for [`relay_rs::runtime::PgPromptQueue`]: idempotent
//! enqueue, claim-and-drain, lease fencing, orphan recovery, attempts cap → poison,
//! pending cap. Each test uses a fresh schema; lease-expiry tests use a `TestClock`
//! so they don't burn wall-clock seconds.

#![allow(clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use relay_rs::clock::{SharedClock, TestClock};
use relay_rs::runtime::queue::PromptQueue as _;
use relay_rs::runtime::{
    IdempotencyKey, LeaseManager as _, LeaseTiming, NewPromptRequest, PgPromptQueue, PromptError,
    PromptRequestId, RequestStatus, WorkerId,
};
use relay_rs::session::{PgSessionStore, SessionId, SessionStore};
use relay_rs::types::Prompt;

mod common;
use common::pg::TestDb;

const LEASE_TTL: Duration = Duration::from_secs(2);
const HEARTBEAT: Duration = Duration::from_millis(500);

/// The returned `TestDb` must be bound (e.g. `let (_db, ...)`) so its `Drop` runs
/// at end-of-scope and reaps the schema; binding it to `_` would drop it immediately.
async fn fresh() -> (TestDb, Arc<PgPromptQueue>, Arc<TestClock>, SessionId) {
    let db = TestDb::fresh().await;
    let test_clock = Arc::new(TestClock::new());
    let clock: SharedClock = test_clock.clone();
    let session_store = PgSessionStore::new(db.pool.clone(), clock.clone());
    let session = session_store.create().await.expect("session");
    let timing = LeaseTiming::try_new(LEASE_TTL, HEARTBEAT).expect("valid timing");
    let queue = Arc::new(PgPromptQueue::with_caps(
        db.pool.clone(),
        clock,
        timing,
        32,
        3,
    ));
    (db, queue, test_clock, session)
}

fn req(session: SessionId, content: &str, key: &str) -> NewPromptRequest {
    NewPromptRequest {
        session,
        content: Prompt::try_from(content).expect("p"),
        idempotency_key: IdempotencyKey::try_from(key).expect("k"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn enqueue_is_idempotent_on_repeat_key() {
    let (_db, q, _c, s) = fresh().await;
    let first = q.enqueue(req(s, "hi", "k1")).await.expect("first");
    let second = q.enqueue(req(s, "hi-again", "k1")).await.expect("second");
    assert_eq!(first.request_id(), second.request_id());
}

#[tokio::test(flavor = "multi_thread")]
async fn claim_drains_all_pending_for_session() {
    let (_db, q, _c, s) = fresh().await;
    let r1 = q.enqueue(req(s, "a", "k1")).await.expect("ok").request_id();
    let r2 = q.enqueue(req(s, "b", "k2")).await.expect("ok").request_id();
    let claimed = q
        .claim_next_session(WorkerId::new())
        .await
        .expect("claim")
        .expect("some");
    assert_eq!(claimed.prompts.len(), 2);
    let ids: Vec<PromptRequestId> = claimed.prompts.iter().map(|p| p.request_id).collect();
    assert!(ids.contains(&r1));
    assert!(ids.contains(&r2));
}

#[tokio::test(flavor = "multi_thread")]
async fn second_claim_skips_leased_session() {
    let (_db, q, _c, s) = fresh().await;
    let _ = q.enqueue(req(s, "a", "k1")).await.expect("ok");
    let _first = q
        .claim_next_session(WorkerId::new())
        .await
        .expect("claim")
        .expect("some");
    let second = q.claim_next_session(WorkerId::new()).await.expect("claim2");
    assert!(second.is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn lease_expiry_returns_orphan_to_pending() {
    let (_db, q, clock, s) = fresh().await;
    let _ = q.enqueue(req(s, "a", "k1")).await.expect("ok");
    let _ = q
        .claim_next_session(WorkerId::new())
        .await
        .expect("claim")
        .expect("some");

    clock.advance(LEASE_TTL + Duration::from_secs(1));

    let again = q
        .claim_next_session(WorkerId::new())
        .await
        .expect("reclaim")
        .expect("orphan recovered");
    assert_eq!(again.prompts.len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn mark_done_with_stale_token_fails() {
    let (_db, q, clock, s) = fresh().await;
    let _ = q.enqueue(req(s, "a", "k1")).await.expect("ok").request_id();
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

    let receipt = claim1.receipt();
    let err = q.mark_done(&receipt).await.expect_err("stale");
    assert!(matches!(err, PromptError::LeaseStale { .. }));
}

#[tokio::test(flavor = "multi_thread")]
async fn poisons_after_max_attempts_via_orphan_path() {
    let db = TestDb::fresh().await;
    let test_clock = Arc::new(TestClock::new());
    let clock: SharedClock = test_clock.clone();
    let session_store = PgSessionStore::new(db.pool.clone(), clock.clone());
    let session = session_store.create().await.expect("session");
    let timing = LeaseTiming::try_new(LEASE_TTL, HEARTBEAT).expect("timing");
    let q = Arc::new(PgPromptQueue::with_caps(
        db.pool.clone(),
        clock,
        timing,
        8,
        2,
    ));
    let r = q
        .enqueue(req(session, "a", "k1"))
        .await
        .expect("ok")
        .request_id();

    // Attempt 1: claim, then let lease expire.
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

#[tokio::test(flavor = "multi_thread")]
async fn heartbeat_extends_lease() {
    let (_db, q, clock, s) = fresh().await;
    let _ = q.enqueue(req(s, "a", "k1")).await.expect("ok");
    let claim = q
        .claim_next_session(WorkerId::new())
        .await
        .expect("c")
        .expect("some");

    clock.advance(LEASE_TTL.saturating_sub(Duration::from_millis(200)));
    q.heartbeat(&claim.lease).await.expect("heartbeat");
    clock.advance(LEASE_TTL.saturating_sub(Duration::from_millis(200)));
    q.heartbeat(&claim.lease).await.expect("heartbeat2");

    q.mark_done(&claim.receipt()).await.expect("done");
}

#[tokio::test(flavor = "multi_thread")]
async fn release_clears_lease_so_others_can_claim() {
    let (db, q, _c, s) = fresh().await;
    let _ = q.enqueue(req(s, "a", "k1")).await.expect("ok");
    let claim = q
        .claim_next_session(WorkerId::new())
        .await
        .expect("c")
        .expect("some");
    q.mark_done(&claim.receipt()).await.expect("done");
    q.release(&claim.lease).await.expect("release");

    let session_store =
        PgSessionStore::new(db.pool.clone(), relay_rs::clock::SystemClock::shared());
    let s2 = session_store.create().await.expect("s2");
    let _ = q.enqueue(req(s2, "b", "k2")).await.expect("ok");
    let again = q.claim_next_session(WorkerId::new()).await.expect("c2");
    assert!(again.is_some());
}

#[tokio::test(flavor = "multi_thread")]
async fn enqueue_caps_pending_per_session() {
    let db = TestDb::fresh().await;
    let test_clock = Arc::new(TestClock::new());
    let clock: SharedClock = test_clock;
    let session_store = PgSessionStore::new(db.pool.clone(), clock.clone());
    let session = session_store.create().await.expect("session");
    let timing = LeaseTiming::try_new(LEASE_TTL, HEARTBEAT).expect("valid timing");
    let q = Arc::new(PgPromptQueue::with_caps(
        db.pool.clone(),
        clock,
        timing,
        2,
        3,
    ));
    q.enqueue(req(session, "a", "k1")).await.expect("ok1");
    q.enqueue(req(session, "b", "k2")).await.expect("ok2");
    let err = q
        .enqueue(req(session, "c", "k3"))
        .await
        .expect_err("over cap");
    assert!(matches!(err, PromptError::PendingCapExceeded { .. }));
}
