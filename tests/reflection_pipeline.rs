//! Trait-contract tests for the reflection pipeline:
//!
//! - The queue's claim path returns `RequestKind::Reflection` rows with
//!   their `kind_payload` and serialises memory-mutating jobs per agent.
//! - `ReflectionScheduler::tick` finds idle sessions whose latest
//!   message is past any existing checkpoint and enqueues exactly one
//!   reflection row.
//! - Repeated ticks against the same idle session do not duplicate.

#![allow(clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use relay_rs::clock::{SharedClock, SystemClock};
use relay_rs::memory::ReflectionScheduler;
use relay_rs::runtime::{
    IdempotencyKey, NewPromptRequest, PgPromptQueue, PromptRequestId, RequestKind,
    RequestKindPayload, SharedPromptQueue, WorkerId,
};
use relay_rs::session::{PgSessionStore, SharedSessionStore};
use relay_rs::types::{Participant, Prompt};

mod common;
use common::pg::{TestDb, human_to_agent_session};

async fn enqueue_reflection_row(
    queue: &SharedPromptQueue,
    session: relay_rs::session::SessionId,
    agent_id: relay_rs::agents::AgentId,
    up_to: PromptRequestId,
    key: &str,
) -> PromptRequestId {
    let req = NewPromptRequest {
        session: Some(session),
        sender: Participant::agent(agent_id),
        receiver_agent_id: agent_id,
        parent_session: None,
        content: Prompt::try_from("(reflection)").expect("p"),
        idempotency_key: IdempotencyKey::try_from(key).expect("k"),
        kind_payload: RequestKindPayload::Reflection {
            session_id: session,
            up_to_turn_id: up_to,
        },
    };
    queue.enqueue(req).await.expect("enqueue").request_id()
}

#[tokio::test(flavor = "multi_thread")]
async fn claim_returns_reflection_kind_and_payload() {
    let db = TestDb::fresh().await;
    let clock: SharedClock = SystemClock::shared();
    let sessions: SharedSessionStore =
        Arc::new(PgSessionStore::new(db.pool.clone(), clock.clone()));
    let queue: SharedPromptQueue = Arc::new(PgPromptQueue::new(db.pool.clone(), clock));

    let session = human_to_agent_session(sessions.as_ref(), db.default_agent_id).await;
    // The reflection's `since_turn_id` references a real prompt row to
    // satisfy the JSON payload's UUID — any id works since the worker
    // does not dereference it during a claim.
    let since = PromptRequestId::new();
    let _ = enqueue_reflection_row(&queue, session, db.default_agent_id, since, "k1").await;

    let claimed = queue
        .claim_next_session(WorkerId::new())
        .await
        .expect("claim")
        .expect("some");
    assert_eq!(claimed.kind_payload.kind(), RequestKind::Reflection);
    match claimed.kind_payload {
        RequestKindPayload::Reflection {
            session_id,
            up_to_turn_id,
        } => {
            assert_eq!(session_id, session);
            assert_eq!(up_to_turn_id, since);
        }
        other => panic!("unexpected payload {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn per_agent_serialization_skips_session_with_in_flight_reflection() {
    // A reflection row in `processing` for agent X must lock out any
    // other claim against agent X — even on a different session of the
    // same agent. This protects the journal from concurrent reflection
    // mutations.
    let db = TestDb::fresh().await;
    let clock: SharedClock = SystemClock::shared();
    let sessions: SharedSessionStore =
        Arc::new(PgSessionStore::new(db.pool.clone(), clock.clone()));
    let queue: SharedPromptQueue = Arc::new(PgPromptQueue::new(db.pool.clone(), clock));

    let agent_id = db.default_agent_id;
    let s1 = human_to_agent_session(sessions.as_ref(), agent_id).await;

    // Enqueue + claim the first reflection — it goes to `processing`
    // and holds a lease.
    let _ = enqueue_reflection_row(&queue, s1, agent_id, PromptRequestId::new(), "k1").await;
    let first = queue
        .claim_next_session(WorkerId::new())
        .await
        .expect("claim")
        .expect("some");
    assert_eq!(first.kind_payload.kind(), RequestKind::Reflection);

    // A normal prompt on a different session for the SAME agent should
    // claim fine — only memory-mutating kinds are serialised per agent.
    let s2_root = PromptRequestId::new();
    let _ = sessions
        .resolve_or_create_for_pair(
            s2_root,
            Participant::Human,
            Participant::agent(agent_id),
            None,
        )
        .await
        .expect("s2");
    let normal = NewPromptRequest::normal(
        None,
        Participant::Human,
        agent_id,
        None,
        Prompt::try_from("hello").expect("p"),
        IdempotencyKey::try_from("normal-k").expect("k"),
    );
    let outcome = queue.enqueue(normal).await.expect("enqueue normal");
    let claim = queue
        .claim_next_session(WorkerId::new())
        .await
        .expect("claim normal")
        .expect("normal claimed");
    assert_eq!(claim.kind_payload.kind(), RequestKind::Normal);
    assert_eq!(claim.session, outcome.session());

    // BUT: a second reflection on a different session for the same
    // agent must be skipped while the first reflection is still
    // processing. (Use s2 which is now bound to the same agent.)
    let _ = enqueue_reflection_row(
        &queue,
        outcome.session(),
        agent_id,
        PromptRequestId::new(),
        "k2",
    )
    .await;
    let second = queue
        .claim_next_session(WorkerId::new())
        .await
        .expect("claim again");
    assert!(
        second.is_none(),
        "second reflection on the same agent must wait, got {second:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn scheduler_tick_enqueues_for_idle_session_with_unprocessed_turns() {
    let db = TestDb::fresh().await;
    let clock: SharedClock = SystemClock::shared();
    let sessions: SharedSessionStore =
        Arc::new(PgSessionStore::new(db.pool.clone(), clock.clone()));
    let queue: SharedPromptQueue = Arc::new(PgPromptQueue::new(db.pool.clone(), clock.clone()));

    // Mint a session, append one message back-dated past the idle
    // threshold so the scheduler's "idle" predicate fires immediately.
    let session = human_to_agent_session(sessions.as_ref(), db.default_agent_id).await;
    let request_id = common::pg::seed_prompt_request(&db.pool, session, db.default_agent_id).await;
    let stale_ts = Utc::now() - chrono::Duration::seconds(60 * 60);
    sqlx::query(
        "INSERT INTO session_messages
             (session_id, seq, request_id, body,
              sender_kind, sender_agent_id,
              receiver_kind, receiver_agent_id, created_at)
         VALUES ($1, 1, $2, $3,
                 'human', NULL,
                 'agent', $4, $5)",
    )
    .bind(session)
    .bind(request_id)
    .bind(serde_json::json!({
        "role": "user",
        "contents": [{"kind": "text", "value": "old message"}]
    }))
    .bind(db.default_agent_id)
    .bind(stale_ts)
    .execute(&db.pool)
    .await
    .expect("seed message");

    // Spawn the scheduler with a tight poll cadence so the test does
    // not have to wait the production 60s. We only need one tick.
    let scheduler = ReflectionScheduler::spawn_with_cadence(
        db.pool.clone(),
        queue.clone(),
        clock,
        Duration::from_millis(100),
        None,
    );
    // Poll until the reflection row appears. The link from conversation to
    // reflection row is `kind_payload.data.session_id`, not
    // `prompt_requests.session_id` (which points at the reflection's own
    // off-conversation session).
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut found = None;
    while std::time::Instant::now() < deadline {
        let row: Option<(uuid::Uuid,)> = sqlx::query_as(
            "SELECT id FROM prompt_requests
             WHERE kind = 'reflection'
               AND kind_payload->'data'->>'session_id' = $1::text
             LIMIT 1",
        )
        .bind(session.as_uuid().to_string())
        .fetch_optional(&db.pool)
        .await
        .expect("query");
        if row.is_some() {
            found = row;
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    scheduler.shutdown().await;
    assert!(
        found.is_some(),
        "scheduler should have enqueued one reflection"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn scheduler_does_not_duplicate_pending_reflection() {
    let db = TestDb::fresh().await;
    let clock: SharedClock = SystemClock::shared();
    let sessions: SharedSessionStore =
        Arc::new(PgSessionStore::new(db.pool.clone(), clock.clone()));
    let queue: SharedPromptQueue = Arc::new(PgPromptQueue::new(db.pool.clone(), clock));
    let session = human_to_agent_session(sessions.as_ref(), db.default_agent_id).await;
    let since = PromptRequestId::new();

    // Pre-seed a reflection row in `pending`. The scheduler's predicate
    // (NOT EXISTS pending/processing reflection) should refuse to add
    // another for the same session.
    let _ = enqueue_reflection_row(&queue, session, db.default_agent_id, since, "preseeded").await;
    let count_before: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM prompt_requests
         WHERE session_id = $1 AND kind = 'reflection'",
    )
    .bind(session)
    .fetch_one(&db.pool)
    .await
    .expect("count");
    assert_eq!(count_before.0, 1);

    // Now back-date a message so the scheduler would otherwise enqueue
    // a fresh reflection. The dedup predicate must keep the count at 1.
    let request_id = common::pg::seed_prompt_request(&db.pool, session, db.default_agent_id).await;
    sqlx::query(
        "INSERT INTO session_messages
             (session_id, seq, request_id, body,
              sender_kind, sender_agent_id,
              receiver_kind, receiver_agent_id, created_at)
         VALUES ($1, 1, $2, $3,
                 'human', NULL,
                 'agent', $4, $5)",
    )
    .bind(session)
    .bind(request_id)
    .bind(serde_json::json!({
        "role": "user",
        "contents": [{"kind": "text", "value": "older"}]
    }))
    .bind(db.default_agent_id)
    .bind(Utc::now() - chrono::Duration::seconds(60 * 60))
    .execute(&db.pool)
    .await
    .expect("seed message");

    let scheduler = ReflectionScheduler::spawn_with_cadence(
        db.pool.clone(),
        queue,
        SystemClock::shared(),
        Duration::from_millis(100),
        None,
    );
    tokio::time::sleep(Duration::from_secs(2)).await;
    scheduler.shutdown().await;

    let count_after: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM prompt_requests
         WHERE session_id = $1 AND kind = 'reflection'",
    )
    .bind(session)
    .fetch_one(&db.pool)
    .await
    .expect("count after");
    assert_eq!(
        count_after.0, 1,
        "scheduler must not duplicate while a pending reflection exists"
    );
}
