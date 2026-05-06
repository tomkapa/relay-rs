//! Behaviour tests for [`relay_rs::memory::AgentMemory`] + the underlying cache.
//!
//! Proves the assembled `system` prompt has the expected `<core>...</core>` /
//! `<role>...</role>` structure and that an admin's edit to an agent row is
//! visible to live workers within the cache TTL.

#![allow(clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use relay_rs::agents::{AgentPromptCache, PgAgentStore, SharedAgentStore};
use relay_rs::clock::{SharedClock, TestClock};
use relay_rs::memory::{
    AgentMemory, CORE_TAG_CLOSE, CORE_TAG_OPEN, Memory, ROLE_TAG_CLOSE, ROLE_TAG_OPEN,
};
use relay_rs::session::{PgSessionStore, SharedSessionStore};

mod common;
use common::pg::TestDb;

const CORE: &str = "be professional and helpful";

fn build_memory(
    db: &TestDb,
    clock: SharedClock,
) -> (AgentMemory, SharedSessionStore, SharedAgentStore) {
    let agents: SharedAgentStore = Arc::new(PgAgentStore::new(db.pool.clone(), clock.clone()));
    let sessions: SharedSessionStore =
        Arc::new(PgSessionStore::new(db.pool.clone(), clock.clone()));
    let cache = Arc::new(AgentPromptCache::new(8, Duration::from_secs(60), clock));
    let memory = AgentMemory::new(sessions.clone(), agents.clone(), cache, CORE);
    (memory, sessions, agents)
}

#[tokio::test(flavor = "multi_thread")]
async fn assembles_core_then_role_in_order() {
    let db = TestDb::fresh().await;
    let clock: SharedClock = Arc::new(TestClock::new());
    let (memory, sessions, _agents) = build_memory(&db, clock);

    let session = sessions.create(db.default_agent_id).await.expect("session");
    let prompt = memory.system_prompt(session).await.expect("system prompt");

    let s = prompt.as_ref();
    let core_open = s.find(CORE_TAG_OPEN).expect("has <core>");
    let core_close = s.find(CORE_TAG_CLOSE).expect("has </core>");
    let role_open = s.find(ROLE_TAG_OPEN).expect("has <role>");
    let role_close = s.find(ROLE_TAG_CLOSE).expect("has </role>");

    // <core> precedes </core>, which precedes <role>, which precedes </role>.
    assert!(core_open < core_close, "core tags ordered");
    assert!(core_close < role_open, "core block precedes role block");
    assert!(role_open < role_close, "role tags ordered");
    assert!(s.contains(CORE), "core text present");
    assert!(
        s.contains("test default prompt"),
        "role text from the seeded agent present"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn cache_serves_within_ttl_then_refreshes_after_expiry() {
    let db = TestDb::fresh().await;
    let clock = Arc::new(TestClock::new());
    let shared: SharedClock = clock.clone();

    // Build the cache directly so the test can drive `clock.advance` past TTL.
    let agents: SharedAgentStore = Arc::new(PgAgentStore::new(db.pool.clone(), shared.clone()));
    let cache = Arc::new(AgentPromptCache::new(
        8,
        Duration::from_secs(60),
        shared.clone(),
    ));

    let first = cache
        .get_or_load(db.default_agent_id, &agents)
        .await
        .expect("load");
    assert_eq!(first.as_str(), "test default prompt");

    // Within TTL: cached value is returned even if we change the row underneath.
    sqlx::query("UPDATE agents SET system_prompt = $1 WHERE id = $2")
        .bind("rolled-out v2")
        .bind(db.default_agent_id)
        .execute(&db.pool)
        .await
        .expect("update");

    clock.advance(Duration::from_secs(30));
    let still_cached = cache
        .get_or_load(db.default_agent_id, &agents)
        .await
        .expect("cached");
    assert_eq!(still_cached.as_str(), "test default prompt");

    // Past TTL: refetch returns the new value.
    clock.advance(Duration::from_secs(31));
    let refreshed = cache
        .get_or_load(db.default_agent_id, &agents)
        .await
        .expect("refreshed");
    assert_eq!(refreshed.as_str(), "rolled-out v2");
}
