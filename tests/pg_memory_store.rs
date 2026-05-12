//! Trait-contract tests for [`relay_rs::memory::PgMemoryStore`]:
//!
//! - journal append correctness
//! - materialized-view consistency
//! - replay rebuild from events
//! - concurrent-mutation safety
//! - `prompt_requests` migration safety (existing rows continue to dispatch
//!   as `Normal`).
//!
//! Each test uses a fresh schema so they can run in parallel.

#![allow(clippy::expect_used)]

use std::sync::Arc;

use relay_rs::clock::SystemClock;
use relay_rs::memory::{
    MemoryContent, MemoryEventPayload, MemoryId, MemoryKind, MemoryMutation, MemoryState,
    MemoryStore, MemoryStoreError, MutationKind, MutationSource, MutationSourceKind, PgMemoryStore,
    ResolutionOutcome, ResolutionReason,
};
use relay_rs::runtime::{PromptRequestId, RequestKind};

mod common;
use common::pg::TestDb;

fn store(db: &TestDb) -> Arc<PgMemoryStore> {
    Arc::new(PgMemoryStore::new(
        db.pool.clone(),
        SystemClock::shared(),
        common::embedding::FakeEmbeddingProvider::shared(),
    ))
}

fn content(s: &str) -> MemoryContent {
    MemoryContent::try_from(s).expect("valid memory content")
}

fn write(
    db: &TestDb,
    kind: MemoryKind,
    body: &str,
    state: MemoryState,
    pinned: bool,
) -> MemoryMutation {
    MemoryMutation::Write {
        agent: db.default_agent_id,
        kind,
        content: content(body),
        state,
        pinned,
        source: MutationSource::Operator,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn write_appends_event_and_materialised_row() {
    let db = TestDb::fresh().await;
    let s = store(&db);

    let outcome = s
        .apply(write(
            &db,
            MemoryKind::Identity,
            "I default to terse replies.",
            MemoryState::Held,
            false,
        ))
        .await
        .expect("apply write");

    let row = outcome.row.expect("write returns row");
    assert_eq!(row.id, outcome.memory_id);
    assert_eq!(row.agent_id, db.default_agent_id);
    assert_eq!(row.kind, MemoryKind::Identity);
    assert_eq!(row.state, MemoryState::Held);
    assert!(!row.pinned);
    assert_eq!(row.content.as_str(), "I default to terse replies.");

    let events = s.list_events(db.default_agent_id).await.expect("events");
    assert_eq!(events.len(), 1);
    let ev = &events[0];
    assert_eq!(ev.id, outcome.event_id);
    assert_eq!(ev.mutation_kind(), MutationKind::Write);
    assert_eq!(ev.target_memory_id, outcome.memory_id);
    let MemoryEventPayload::Write { content, kind, .. } = &ev.payload else {
        panic!("expected Write payload, got {:?}", ev.payload);
    };
    assert_eq!(content.as_str(), "I default to terse replies.");
    assert_eq!(*kind, MemoryKind::Identity);
    assert_eq!(ev.source.kind(), MutationSourceKind::Operator);

    let listed = s.list(db.default_agent_id).await.expect("list");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, outcome.memory_id);
}

#[tokio::test(flavor = "multi_thread")]
async fn update_records_before_and_after_content() {
    let db = TestDb::fresh().await;
    let s = store(&db);

    let first = s
        .apply(write(
            &db,
            MemoryKind::Other,
            "translator is fast on European languages",
            MemoryState::Tentative,
            false,
        ))
        .await
        .expect("write");

    let updated = s
        .apply(MemoryMutation::Update {
            agent: db.default_agent_id,
            target: first.memory_id,
            content: content("translator is fast on Romance languages"),
            state: MemoryState::Tentative,
            source: MutationSource::Operator,
        })
        .await
        .expect("update");

    let row = updated.row.expect("update returns row");
    assert_eq!(row.id, first.memory_id);
    assert_eq!(
        row.content.as_str(),
        "translator is fast on Romance languages"
    );

    let events = s.list_events(db.default_agent_id).await.expect("events");
    assert_eq!(events.len(), 2);
    let upd = &events[1];
    assert_eq!(upd.mutation_kind(), MutationKind::Update);
    let MemoryEventPayload::Update { before, after, .. } = &upd.payload else {
        panic!("expected Update payload, got {:?}", upd.payload);
    };
    assert_eq!(before.as_str(), "translator is fast on European languages");
    assert_eq!(after.as_str(), "translator is fast on Romance languages");
}

#[tokio::test(flavor = "multi_thread")]
async fn forget_removes_materialised_but_journal_keeps_event() {
    let db = TestDb::fresh().await;
    let s = store(&db);

    let written = s
        .apply(write(
            &db,
            MemoryKind::Procedure,
            "ask one focused clarifying question",
            MemoryState::Held,
            false,
        ))
        .await
        .expect("write");

    let forget = s
        .apply(MemoryMutation::Forget {
            agent: db.default_agent_id,
            target: written.memory_id,
            source: MutationSource::Librarian,
        })
        .await
        .expect("forget");
    assert!(forget.row.is_none());

    assert!(
        s.get(written.memory_id).await.expect("get").is_none(),
        "row should be gone from materialised view"
    );

    let events = s.list_events(db.default_agent_id).await.expect("events");
    assert_eq!(events.len(), 2);
    let f = &events[1];
    assert_eq!(f.mutation_kind(), MutationKind::Forget);
    let MemoryEventPayload::Forget { before } = &f.payload else {
        panic!("expected Forget payload, got {:?}", f.payload);
    };
    assert_eq!(before.as_str(), "ask one focused clarifying question");
    assert_eq!(f.source.kind(), MutationSourceKind::Librarian);
}

#[tokio::test(flavor = "multi_thread")]
async fn update_unknown_id_is_not_found() {
    let db = TestDb::fresh().await;
    let s = store(&db);

    let stranger = MemoryId::new();
    let err = s
        .apply(MemoryMutation::Update {
            agent: db.default_agent_id,
            target: stranger,
            content: content("nope"),
            state: MemoryState::Tentative,
            source: MutationSource::Operator,
        })
        .await
        .expect_err("should fail");
    assert!(matches!(err, MemoryStoreError::NotFound { .. }));
}

#[tokio::test(flavor = "multi_thread")]
async fn pinned_row_rejects_agent_update_but_accepts_operator_override() {
    let db = TestDb::fresh().await;
    let s = store(&db);

    let pinned = s
        .apply(write(
            &db,
            MemoryKind::Identity,
            "operator pinned identity",
            MemoryState::Core,
            true,
        ))
        .await
        .expect("write pinned");

    let agent_err = s
        .apply(MemoryMutation::Update {
            agent: db.default_agent_id,
            target: pinned.memory_id,
            content: content("agent attempt"),
            state: MemoryState::Tentative,
            source: MutationSource::Turn(PromptRequestId::new()),
        })
        .await
        .expect_err("agent edit blocked");
    assert!(matches!(
        agent_err,
        MemoryStoreError::PinnedImmutable { .. }
    ));

    let _ok = s
        .apply(MemoryMutation::Update {
            agent: db.default_agent_id,
            target: pinned.memory_id,
            content: content("operator override"),
            state: MemoryState::Core,
            source: MutationSource::Operator,
        })
        .await
        .expect("operator override succeeds");
}

#[tokio::test(flavor = "multi_thread")]
async fn rebuild_materialized_reproduces_live_view() {
    let db = TestDb::fresh().await;
    let s = store(&db);

    let a = s
        .apply(write(
            &db,
            MemoryKind::Identity,
            "first",
            MemoryState::Held,
            false,
        ))
        .await
        .expect("a");
    let _b = s
        .apply(write(
            &db,
            MemoryKind::Other,
            "second",
            MemoryState::Tentative,
            false,
        ))
        .await
        .expect("b");
    let c = s
        .apply(write(
            &db,
            MemoryKind::Procedure,
            "third",
            MemoryState::Held,
            false,
        ))
        .await
        .expect("c");

    s.apply(MemoryMutation::Update {
        agent: db.default_agent_id,
        target: a.memory_id,
        content: content("first revised"),
        state: MemoryState::Tentative,
        source: MutationSource::Operator,
    })
    .await
    .expect("update");

    s.apply(MemoryMutation::Forget {
        agent: db.default_agent_id,
        target: c.memory_id,
        source: MutationSource::Operator,
    })
    .await
    .expect("forget");

    let live_ids: Vec<MemoryId> = s
        .list(db.default_agent_id)
        .await
        .expect("list")
        .iter()
        .map(|r| r.id)
        .collect();
    let live_contents: Vec<String> = s
        .list(db.default_agent_id)
        .await
        .expect("list")
        .into_iter()
        .map(|r| r.content.as_str().to_owned())
        .collect();

    s.rebuild_materialized(db.default_agent_id)
        .await
        .expect("rebuild");

    let rebuilt_ids: Vec<MemoryId> = s
        .list(db.default_agent_id)
        .await
        .expect("list2")
        .iter()
        .map(|r| r.id)
        .collect();
    let rebuilt_contents: Vec<String> = s
        .list(db.default_agent_id)
        .await
        .expect("list2")
        .into_iter()
        .map(|r| r.content.as_str().to_owned())
        .collect();

    assert_eq!(live_ids, rebuilt_ids);
    assert_eq!(live_contents, rebuilt_contents);
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_writes_are_independent() {
    // Different (agent, target) tuples must not contend; the journal is
    // append-only and each write mints a fresh memory id, so two parallel
    // writers should both succeed without retries.
    let db = TestDb::fresh().await;
    let s = store(&db);

    let s1 = s.clone();
    let s2 = s.clone();
    let agent = db.default_agent_id;

    let h1 = tokio::spawn(async move {
        s1.apply(MemoryMutation::Write {
            agent,
            kind: MemoryKind::Identity,
            content: content("one"),
            state: MemoryState::Held,
            pinned: false,
            source: MutationSource::Operator,
        })
        .await
    });
    let h2 = tokio::spawn(async move {
        s2.apply(MemoryMutation::Write {
            agent,
            kind: MemoryKind::Other,
            content: content("two"),
            state: MemoryState::Tentative,
            pinned: false,
            source: MutationSource::Operator,
        })
        .await
    });

    let r1 = h1.await.expect("join1").expect("apply1");
    let r2 = h2.await.expect("join2").expect("apply2");
    assert_ne!(r1.memory_id, r2.memory_id);

    let listed = s.list(agent).await.expect("list");
    assert_eq!(listed.len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn prompt_requests_kind_defaults_to_normal() {
    // Pre-existing inserters do not specify `kind` or `kind_payload`; the
    // column defaults backfill `'normal'` and the empty `Normal` payload
    // so the worker dispatches them on the existing reply path. This
    // locks in the migration-safety invariant called out in §2.1:
    // existing rows continue to dispatch as `Normal`.
    let db = TestDb::fresh().await;

    let session_store = relay_rs::session::PgSessionStore::new(
        db.pool.clone(),
        relay_rs::clock::SystemClock::shared(),
    );
    let session = common::pg::human_to_agent_session(&session_store, db.default_agent_id).await;
    let id = common::pg::seed_prompt_request(&db.pool, session, db.default_agent_id).await;

    let (kind, payload): (RequestKind, serde_json::Value) =
        sqlx::query_as("SELECT kind, kind_payload FROM prompt_requests WHERE id = $1")
            .bind(id)
            .fetch_one(&db.pool)
            .await
            .expect("fetch");

    assert_eq!(kind, RequestKind::Normal);
    assert_eq!(
        payload,
        serde_json::json!({"kind": "normal", "data": {}}),
        "default backfill should produce the Normal payload"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn record_validation_promotes_state() {
    use relay_rs::memory::ValidationOrigin;

    let db = TestDb::fresh().await;
    let s = store(&db);

    // Tentative write via operator path so the source_turn_id FK is NULL.
    let outcome = s
        .apply(MemoryMutation::Write {
            agent: db.default_agent_id,
            kind: MemoryKind::Identity,
            content: content("Tabs over spaces"),
            state: MemoryState::Tentative,
            pinned: false,
            source: MutationSource::Operator,
        })
        .await
        .expect("write");

    let row = s
        .record_validation(
            db.default_agent_id,
            outcome.memory_id,
            ValidationOrigin::Librarian,
            Some("librarian dedup match"),
        )
        .await
        .expect("record");
    assert_eq!(row.state, MemoryState::Held);

    // Second validation promotes Held -> Validated
    let row = s
        .record_validation(
            db.default_agent_id,
            outcome.memory_id,
            ValidationOrigin::Operator,
            None,
        )
        .await
        .expect("record");
    assert_eq!(row.state, MemoryState::Validated);
}

#[tokio::test(flavor = "multi_thread")]
async fn revert_event_undoes_write() {
    let db = TestDb::fresh().await;
    let s = store(&db);

    let outcome = s
        .apply(MemoryMutation::Write {
            agent: db.default_agent_id,
            kind: MemoryKind::Identity,
            content: content("Ephemeral"),
            state: MemoryState::Tentative,
            pinned: false,
            source: MutationSource::Operator,
        })
        .await
        .expect("write");

    let revert = s
        .revert_event(db.default_agent_id, outcome.event_id)
        .await
        .expect("revert");
    assert!(revert.is_none(), "row removed by revert");
}

#[tokio::test(flavor = "multi_thread")]
async fn evict_overflow_drops_below_quota() {
    let db = TestDb::fresh().await;
    let s = store(&db);

    for i in 0..5 {
        s.apply(MemoryMutation::Write {
            agent: db.default_agent_id,
            kind: MemoryKind::Identity,
            content: content(&format!("memory {i}")),
            state: MemoryState::Tentative,
            pinned: false,
            source: MutationSource::Operator,
        })
        .await
        .expect("write");
    }

    let evicted = s
        .evict_overflow(db.default_agent_id, 3)
        .await
        .expect("evict");
    assert_eq!(evicted.len(), 2);
    let listed = s.list(db.default_agent_id).await.expect("list");
    assert_eq!(listed.len(), 3);
}

#[tokio::test(flavor = "multi_thread")]
async fn decay_demotes_old_validated() {
    let db = TestDb::fresh().await;
    let s = store(&db);

    let outcome = s
        .apply(MemoryMutation::Write {
            agent: db.default_agent_id,
            kind: MemoryKind::Identity,
            content: content("decaying"),
            state: MemoryState::Validated,
            pinned: false,
            source: MutationSource::Operator,
        })
        .await
        .expect("write");

    // Cutoff in the future demotes everything Validated.
    let future = chrono::Utc::now() + chrono::Duration::seconds(1);
    let n = s
        .decay_validated(db.default_agent_id, future)
        .await
        .expect("decay");
    assert_eq!(n, 1);
    let row = s.get(outcome.memory_id).await.expect("get").expect("row");
    assert_eq!(row.state, MemoryState::Held);
}

#[tokio::test(flavor = "multi_thread")]
async fn record_contradiction_is_idempotent() {
    let db = TestDb::fresh().await;
    let s = store(&db);

    let a = s
        .apply(MemoryMutation::Write {
            agent: db.default_agent_id,
            kind: MemoryKind::Identity,
            content: content("ship on Friday"),
            state: MemoryState::Held,
            pinned: false,
            source: MutationSource::Operator,
        })
        .await
        .expect("a");
    let b = s
        .apply(MemoryMutation::Write {
            agent: db.default_agent_id,
            kind: MemoryKind::Identity,
            content: content("don't ship on Friday"),
            state: MemoryState::Held,
            pinned: false,
            source: MutationSource::Operator,
        })
        .await
        .expect("b");

    let id1 = s
        .record_contradiction(db.default_agent_id, a.memory_id, b.memory_id, "test")
        .await
        .expect("c1");
    let id2 = s
        .record_contradiction(db.default_agent_id, a.memory_id, b.memory_id, "again")
        .await
        .expect("c2");
    assert_eq!(id1, id2);
}

#[tokio::test(flavor = "multi_thread")]
async fn set_pinned_toggles_protection() {
    let db = TestDb::fresh().await;
    let s = store(&db);

    let outcome = s
        .apply(MemoryMutation::Write {
            agent: db.default_agent_id,
            kind: MemoryKind::Identity,
            content: content("invariant"),
            state: MemoryState::Held,
            pinned: false,
            source: MutationSource::Operator,
        })
        .await
        .expect("w");

    let row = s
        .set_pinned(db.default_agent_id, outcome.memory_id, true)
        .await
        .expect("pin");
    assert!(row.pinned);

    // Agent path now rejects.
    let err = s
        .apply(MemoryMutation::Update {
            agent: db.default_agent_id,
            target: outcome.memory_id,
            content: content("changed"),
            state: MemoryState::Tentative,
            source: MutationSource::Turn(PromptRequestId::new()),
        })
        .await
        .expect_err("pinned protects");
    assert!(matches!(err, MemoryStoreError::PinnedImmutable { .. }));
}

async fn seed_contradiction(
    s: &Arc<PgMemoryStore>,
    db: &TestDb,
) -> relay_rs::memory::ContradictionEventId {
    let a = s
        .apply(MemoryMutation::Write {
            agent: db.default_agent_id,
            kind: MemoryKind::Identity,
            content: content("ship on Friday"),
            state: MemoryState::Held,
            pinned: false,
            source: MutationSource::Operator,
        })
        .await
        .expect("a");
    let b = s
        .apply(MemoryMutation::Write {
            agent: db.default_agent_id,
            kind: MemoryKind::Identity,
            content: content("don't ship on Friday"),
            state: MemoryState::Held,
            pinned: false,
            source: MutationSource::Operator,
        })
        .await
        .expect("b");
    s.record_contradiction(db.default_agent_id, a.memory_id, b.memory_id, "test")
        .await
        .expect("contradiction")
}

#[tokio::test(flavor = "multi_thread")]
async fn resolve_contradiction_mutation_close_links_event() {
    let db = TestDb::fresh().await;
    let s = store(&db);
    let id = seed_contradiction(&s, &db).await;

    // Mint a memory event to point at — any apply produces one.
    // Use Operator source so we don't need to seed a prompt_requests row.
    let event = s
        .apply(MemoryMutation::Write {
            agent: db.default_agent_id,
            kind: MemoryKind::Procedure,
            content: content("revised procedure"),
            state: MemoryState::Tentative,
            pinned: false,
            source: MutationSource::Operator,
        })
        .await
        .expect("event");

    s.resolve_contradiction(id, ResolutionOutcome::Mutation(event.event_id))
        .await
        .expect("close");

    let row = s.read_contradiction(id).await.expect("read").expect("row");
    assert!(row.resolved_at.is_some());
    assert_eq!(row.resolution_event_id, Some(event.event_id));
    assert_eq!(row.resolution_reason, None);
}

#[tokio::test(flavor = "multi_thread")]
async fn resolve_contradiction_no_action_close_persists_reason() {
    let db = TestDb::fresh().await;
    let s = store(&db);
    let id = seed_contradiction(&s, &db).await;

    let reason = ResolutionReason::try_from(
        "Both memories are correct in different contexts; no mutation needed.".to_string(),
    )
    .expect("reason");
    s.resolve_contradiction(id, ResolutionOutcome::NoAction { reason })
        .await
        .expect("close");

    let row = s.read_contradiction(id).await.expect("read").expect("row");
    assert!(row.resolved_at.is_some());
    assert_eq!(row.resolution_event_id, None);
    assert_eq!(
        row.resolution_reason.as_deref(),
        Some("Both memories are correct in different contexts; no mutation needed.")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn resolve_contradiction_is_idempotent() {
    let db = TestDb::fresh().await;
    let s = store(&db);
    let id = seed_contradiction(&s, &db).await;

    let reason = ResolutionReason::try_from("first close".to_string()).expect("reason");
    s.resolve_contradiction(id, ResolutionOutcome::NoAction { reason })
        .await
        .expect("close 1");

    let first = s.read_contradiction(id).await.expect("r1").expect("r1");
    let stamped_at = first.resolved_at.expect("resolved");

    // Second call should be a no-op — the stored stamp does not change.
    let again = ResolutionReason::try_from("second close attempt".to_string()).expect("reason");
    s.resolve_contradiction(id, ResolutionOutcome::NoAction { reason: again })
        .await
        .expect("close 2");

    let after = s.read_contradiction(id).await.expect("r2").expect("r2");
    assert_eq!(after.resolved_at, Some(stamped_at));
    assert_eq!(after.resolution_reason.as_deref(), Some("first close"));
}

#[test]
fn resolution_reason_rejects_empty_and_oversize() {
    assert!(ResolutionReason::try_from(String::new()).is_err());
    let max = relay_rs::memory::CONTRADICTION_REASON_MAX_BYTES;
    let ok = "x".repeat(max);
    assert!(ResolutionReason::try_from(ok).is_ok());
    let too_long = "x".repeat(max + 1);
    assert!(ResolutionReason::try_from(too_long).is_err());
}
