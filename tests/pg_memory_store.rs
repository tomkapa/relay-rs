//! Trait-contract tests for [`relay_rs::memory::PgMemoryStore`] (doc/memory.md
//! §2.1, Phase 1):
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
    MemoryContent, MemoryId, MemoryKind, MemoryMutation, MemoryState, MemoryStore,
    MemoryStoreError, MutationKind, MutationSource, MutationSourceKind, PgMemoryStore,
};
use relay_rs::runtime::{PromptRequestId, RequestKind};

mod common;
use common::pg::TestDb;

fn store(db: &TestDb) -> Arc<PgMemoryStore> {
    Arc::new(PgMemoryStore::new(db.pool.clone(), SystemClock::shared()))
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
    assert_eq!(ev.mutation, MutationKind::Write);
    assert_eq!(ev.target_memory_id, outcome.memory_id);
    assert!(ev.content_before.is_none());
    assert_eq!(
        ev.content_after
            .as_ref()
            .expect("write has content")
            .as_str(),
        "I default to terse replies."
    );
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
            operator_override: false,
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
    assert_eq!(upd.mutation, MutationKind::Update);
    assert_eq!(
        upd.content_before.as_ref().expect("before").as_str(),
        "translator is fast on European languages"
    );
    assert_eq!(
        upd.content_after.as_ref().expect("after").as_str(),
        "translator is fast on Romance languages"
    );
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
            operator_override: false,
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
    assert_eq!(f.mutation, MutationKind::Forget);
    assert_eq!(
        f.content_before.as_ref().expect("before").as_str(),
        "ask one focused clarifying question"
    );
    assert!(f.content_after.is_none());
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
            operator_override: false,
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
            operator_override: false,
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
            operator_override: true,
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
        operator_override: false,
    })
    .await
    .expect("update");

    s.apply(MemoryMutation::Forget {
        agent: db.default_agent_id,
        target: c.memory_id,
        source: MutationSource::Operator,
        operator_override: false,
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
    // Pre-existing inserters do not specify `kind`; the column default
    // backfills `'normal'` so the worker dispatches them on the existing
    // reply path. This locks in the migration-safety invariant called out
    // in §2.1: existing rows continue to dispatch as `Normal`.
    let db = TestDb::fresh().await;

    let session_store = relay_rs::session::PgSessionStore::new(
        db.pool.clone(),
        relay_rs::clock::SystemClock::shared(),
    );
    let session = common::pg::human_to_agent_session(&session_store, db.default_agent_id).await;
    let id = common::pg::seed_prompt_request(&db.pool, session, db.default_agent_id).await;

    let (kind, payload): (RequestKind, Option<serde_json::Value>) =
        sqlx::query_as("SELECT kind, kind_payload FROM prompt_requests WHERE id = $1")
            .bind(id)
            .fetch_one(&db.pool)
            .await
            .expect("fetch");

    assert_eq!(kind, RequestKind::Normal);
    assert!(payload.is_none());
}
