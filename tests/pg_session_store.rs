//! Trait-contract tests for [`relay_rs::session::PgSessionStore`]. Each test owns its
//! own schema via `TestDb::fresh` so they can run in parallel.

#![allow(clippy::expect_used)]

use std::sync::Arc;

use relay_rs::agents::AgentId;
use relay_rs::clock::SystemClock;
use relay_rs::provider::{ChatMessage, UserContent};
use relay_rs::session::{PgSessionStore, SessionError, SessionId, SessionStore};

mod common;
use common::pg::TestDb;

fn store(db: &TestDb) -> Arc<PgSessionStore> {
    Arc::new(PgSessionStore::new(db.pool.clone(), SystemClock::shared()))
}

#[tokio::test(flavor = "multi_thread")]
async fn create_append_snapshot_roundtrip() {
    let db = TestDb::fresh().await;
    let store = store(&db);

    let id = store.create(db.default_agent_id).await.expect("create");
    store
        .append(id, ChatMessage::User(vec![UserContent::Text("hi".into())]))
        .await
        .expect("append");
    store
        .append(
            id,
            ChatMessage::User(vec![UserContent::Text("again".into())]),
        )
        .await
        .expect("append2");

    let snap = store.snapshot(id).await.expect("snapshot");
    assert_eq!(snap.len(), 2);
    let ChatMessage::User(contents) = &snap[0] else {
        panic!("first message should be user");
    };
    assert!(matches!(&contents[0], UserContent::Text(t) if t == "hi"));
}

#[tokio::test(flavor = "multi_thread")]
async fn missing_session_is_not_found() {
    let db = TestDb::fresh().await;
    let store = store(&db);

    let id = SessionId::new();
    let err = store.snapshot(id).await.expect_err("absent");
    assert!(matches!(err, SessionError::NotFound(_)));

    let err = store
        .append(id, ChatMessage::User(vec![UserContent::Text("hi".into())]))
        .await
        .expect_err("absent append");
    assert!(matches!(err, SessionError::NotFound(_)));

    let err = store.delete(id).await.expect_err("absent delete");
    assert!(matches!(err, SessionError::NotFound(_)));
}

#[tokio::test(flavor = "multi_thread")]
async fn enforces_message_cap() {
    let db = TestDb::fresh().await;
    let store = Arc::new(PgSessionStore::with_caps(
        db.pool.clone(),
        SystemClock::shared(),
        2,
    ));
    let id = store.create(db.default_agent_id).await.expect("create");
    for _ in 0..2 {
        store
            .append(id, ChatMessage::User(vec![UserContent::Text("x".into())]))
            .await
            .expect("under cap");
    }
    let err = store
        .append(
            id,
            ChatMessage::User(vec![UserContent::Text("over".into())]),
        )
        .await
        .expect_err("at cap");
    assert!(matches!(err, SessionError::MessageCapExceeded { .. }));
}

#[tokio::test(flavor = "multi_thread")]
async fn delete_cascades_messages() {
    let db = TestDb::fresh().await;
    let store = store(&db);
    let id = store.create(db.default_agent_id).await.expect("create");
    store
        .append(id, ChatMessage::User(vec![UserContent::Text("hi".into())]))
        .await
        .expect("append");

    store.delete(id).await.expect("delete");
    let err = store.snapshot(id).await.expect_err("gone");
    assert!(matches!(err, SessionError::NotFound(_)));
}

#[tokio::test(flavor = "multi_thread")]
async fn create_with_unknown_agent_returns_agent_not_found() {
    let db = TestDb::fresh().await;
    let store = store(&db);

    // Random uuid that does not exist in the agents table — the FK on
    // sessions.agent_id should reject the insert and surface as
    // SessionError::AgentNotFound.
    let phantom = AgentId::new();
    let err = store.create(phantom).await.expect_err("fk");
    assert!(matches!(err, SessionError::AgentNotFound(_)));
}

#[tokio::test(flavor = "multi_thread")]
async fn agent_id_round_trips_through_session() {
    let db = TestDb::fresh().await;
    let store = store(&db);

    let id = store.create(db.default_agent_id).await.expect("create");
    let resolved = store.agent_id(id).await.expect("resolve");
    assert_eq!(resolved, db.default_agent_id);
}
