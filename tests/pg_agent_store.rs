//! Trait-contract tests for [`relay_rs::agents::PgAgentStore`]: idempotent
//! seeding, default lookup, missing-agent error, and read round-trip.

#![allow(clippy::expect_used)]

use std::sync::Arc;

use relay_rs::agents::{
    AgentId, AgentName, AgentStore, AgentStoreError, AgentSystemPrompt, DefaultAgentSeed,
    PgAgentStore,
};
use relay_rs::clock::SystemClock;

mod common;
use common::pg::TestDb;

fn store(db: &TestDb) -> Arc<PgAgentStore> {
    Arc::new(PgAgentStore::new(db.pool.clone(), SystemClock::shared()))
}

fn seed(name: &str, prompt: &str) -> DefaultAgentSeed {
    DefaultAgentSeed {
        name: AgentName::try_from(name).expect("valid name"),
        system_prompt: AgentSystemPrompt::try_from(prompt).expect("valid prompt"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn seed_default_is_idempotent() {
    let db = TestDb::fresh().await;
    let store = store(&db);

    // First seed: TestDb::fresh already inserted one. A second call must return
    // the same id rather than minting a new row.
    let again = store
        .seed_default(seed("ignored", "ignored"))
        .await
        .expect("seed again");
    assert_eq!(again, db.default_agent_id);

    // Third call from a totally fresh seed payload still resolves to the same row.
    let third = store
        .seed_default(seed("also-ignored", "also-ignored"))
        .await
        .expect("seed third");
    assert_eq!(third, db.default_agent_id);
}

#[tokio::test(flavor = "multi_thread")]
async fn seed_default_does_not_overwrite_existing_prompt() {
    let db = TestDb::fresh().await;
    let store = store(&db);

    // Re-seed with a different prompt; the existing row's prompt must be
    // preserved per the design conversation ("seed-only, no overwrite").
    let _ = store
        .seed_default(seed("new-name", "this should be ignored"))
        .await
        .expect("seed again");

    let record = store.read(db.default_agent_id).await.expect("read");
    assert!(record.is_default);
    // Original prompt from TestDb's seed wins.
    assert_eq!(record.system_prompt.as_str(), "test default prompt");
    assert_eq!(record.name.as_str(), "test-default");
}

#[tokio::test(flavor = "multi_thread")]
async fn read_unknown_returns_not_found() {
    let db = TestDb::fresh().await;
    let store = store(&db);

    let phantom = AgentId::new();
    let err = store.read(phantom).await.expect_err("not present");
    assert!(matches!(err, AgentStoreError::NotFound(_)));
}

#[tokio::test(flavor = "multi_thread")]
async fn default_id_returns_seeded_row() {
    let db = TestDb::fresh().await;
    let store = store(&db);

    let id = store.default_id().await.expect("default");
    assert_eq!(id, db.default_agent_id);
}
