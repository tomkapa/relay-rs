//! Behaviour tests for [`relay_rs::memory::AgentMemory`] + the underlying
//! caches and composer (doc/memory.md §2.2 — Phase 2).
//!
//! Proves the assembled `system` prompt has the expected
//! `<core>...</core>` / `<role>...</role>` / `<memory>...</memory>`
//! structure, that an admin's edit to an agent row is visible to live
//! workers within the cache TTL, and that the per-session memory section
//! is frozen for the session's lifetime.

#![allow(clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use relay_rs::agents::{AgentPromptCache, PgAgentStore, SharedAgentStore};
use relay_rs::clock::{SharedClock, SystemClock, TestClock};
use relay_rs::memory::{
    AgentMemory, CORE_TAG_CLOSE, CORE_TAG_OPEN, MEMORY_TAG_CLOSE, MEMORY_TAG_OPEN, Memory,
    MemoryContent, MemoryHandle, MemoryKind, MemoryMutation, MemoryState, MutationSource,
    PgMemoryStore, ROLE_TAG_CLOSE, ROLE_TAG_OPEN, SessionMemoryCache, SharedMemoryStore,
};
use relay_rs::session::{PgSessionStore, SharedSessionStore};
use relay_rs::types::Participant;

mod common;
use common::pg::{TestDb, human_to_agent_session};

const CORE: &str = "be professional and helpful";

struct Fixture {
    memory: AgentMemory,
    sessions: SharedSessionStore,
    store: SharedMemoryStore,
}

fn build_memory(db: &TestDb, clock: SharedClock) -> Fixture {
    let agents: SharedAgentStore = Arc::new(PgAgentStore::new(db.pool.clone(), clock.clone()));
    let sessions: SharedSessionStore =
        Arc::new(PgSessionStore::new(db.pool.clone(), clock.clone()));
    let prompt_cache = AgentPromptCache::new(8, Duration::from_secs(60), clock.clone());
    let store: SharedMemoryStore = Arc::new(PgMemoryStore::new(db.pool.clone(), clock.clone()));
    let session_cache = SessionMemoryCache::new(16, Duration::from_secs(60), clock.clone());
    let memory = AgentMemory::new(
        agents.clone(),
        prompt_cache,
        store.clone(),
        session_cache,
        CORE,
    );
    Fixture {
        memory,
        sessions,
        store,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn assembles_core_then_role_in_order() {
    let db = TestDb::fresh().await;
    let clock: SharedClock = Arc::new(TestClock::new());
    let f = build_memory(&db, clock);

    let session = human_to_agent_session(f.sessions.as_ref(), db.default_agent_id).await;
    let viewer = Participant::agent(db.default_agent_id);
    let prompt = f
        .memory
        .system_prompt(session, viewer)
        .await
        .expect("system prompt");

    let s = prompt.as_ref();
    let core_open = s.find(CORE_TAG_OPEN).expect("has <core>");
    let core_close = s.find(CORE_TAG_CLOSE).expect("has </core>");
    let role_open = s.find(ROLE_TAG_OPEN).expect("has <role>");
    let role_close = s.find(ROLE_TAG_CLOSE).expect("has </role>");

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
async fn empty_memory_skips_memory_section() {
    let db = TestDb::fresh().await;
    let clock: SharedClock = Arc::new(TestClock::new());
    let f = build_memory(&db, clock);

    let session = human_to_agent_session(f.sessions.as_ref(), db.default_agent_id).await;
    let prompt = f
        .memory
        .system_prompt(session, Participant::agent(db.default_agent_id))
        .await
        .expect("system prompt");

    assert!(
        !prompt.contains(MEMORY_TAG_OPEN),
        "no memory tag when no memories: {prompt}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn renders_memory_section_after_role() {
    let db = TestDb::fresh().await;
    let clock: SharedClock = Arc::new(TestClock::new());
    let f = build_memory(&db, clock);
    let agent_id = db.default_agent_id;

    f.store
        .apply(MemoryMutation::Write {
            agent: agent_id,
            kind: MemoryKind::Identity,
            content: MemoryContent::try_from("I default to terse replies.").expect("valid"),
            state: MemoryState::Validated,
            pinned: false,
            source: MutationSource::Operator,
        })
        .await
        .expect("write");

    let session = human_to_agent_session(f.sessions.as_ref(), agent_id).await;
    let prompt = f
        .memory
        .system_prompt(session, Participant::agent(agent_id))
        .await
        .expect("system prompt");

    let s = prompt.as_ref();
    let role_close = s.find(ROLE_TAG_CLOSE).expect("</role>");
    let memory_open = s.find(MEMORY_TAG_OPEN).expect("<memory>");
    let memory_close = s.find(MEMORY_TAG_CLOSE).expect("</memory>");

    assert!(role_close < memory_open, "memory follows role: {s}");
    assert!(memory_open < memory_close, "memory tags ordered");
    assert!(
        s.contains("- [M-1, validated] I default to terse replies."),
        "memory line shape: {s}"
    );
    assert!(s.contains("### Self"));
}

#[tokio::test(flavor = "multi_thread")]
async fn frozen_during_session_returns_identical_prompt() {
    // The composed memory section must be cached for the session's
    // lifetime so the prompt prefix stays stable across turns. Adding a
    // memory between two `system_prompt` calls in the same session must
    // not change the second call's output.
    let db = TestDb::fresh().await;
    let clock: SharedClock = Arc::new(TestClock::new());
    let f = build_memory(&db, clock);
    let agent_id = db.default_agent_id;
    let session = human_to_agent_session(f.sessions.as_ref(), agent_id).await;
    let viewer = Participant::agent(agent_id);

    let first = f
        .memory
        .system_prompt(session, viewer)
        .await
        .expect("first");

    f.store
        .apply(MemoryMutation::Write {
            agent: agent_id,
            kind: MemoryKind::Identity,
            content: MemoryContent::try_from("post-cache memory").expect("valid"),
            state: MemoryState::Tentative,
            pinned: false,
            source: MutationSource::Operator,
        })
        .await
        .expect("write");

    let second = f
        .memory
        .system_prompt(session, viewer)
        .await
        .expect("second");

    assert_eq!(
        first.as_ref(),
        second.as_ref(),
        "prompt frozen for the session's lifetime"
    );
    assert!(
        !second.contains("post-cache memory"),
        "post-cache write must not leak into the cached section: {second}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn resolve_handle_round_trips_to_memory_id() {
    let db = TestDb::fresh().await;
    let clock: SharedClock = Arc::new(TestClock::new());
    let f = build_memory(&db, clock);
    let agent_id = db.default_agent_id;

    let outcome = f
        .store
        .apply(MemoryMutation::Write {
            agent: agent_id,
            kind: MemoryKind::Identity,
            content: MemoryContent::try_from("identity").expect("valid"),
            state: MemoryState::Held,
            pinned: false,
            source: MutationSource::Operator,
        })
        .await
        .expect("write");

    let session = human_to_agent_session(f.sessions.as_ref(), agent_id).await;
    // Compose the section so the handle map is populated.
    let _ = f
        .memory
        .system_prompt(session, Participant::agent(agent_id))
        .await
        .expect("compose");

    let handle = MemoryHandle::try_from(1u32).expect("valid");
    let resolved = f
        .memory
        .resolve_handle(session, agent_id, handle)
        .await
        .expect("resolve");
    assert_eq!(resolved, Some(outcome.memory_id));

    let stranger = MemoryHandle::try_from(999u32).expect("valid");
    let missing = f
        .memory
        .resolve_handle(session, agent_id, stranger)
        .await
        .expect("resolve missing");
    assert_eq!(missing, None);
}

#[tokio::test(flavor = "multi_thread")]
async fn cache_serves_within_ttl_then_refreshes_after_expiry() {
    let db = TestDb::fresh().await;
    let clock = Arc::new(TestClock::new());
    let shared: SharedClock = clock.clone();

    let agents: SharedAgentStore = Arc::new(PgAgentStore::new(db.pool.clone(), shared.clone()));
    let cache = AgentPromptCache::new(8, Duration::from_secs(60), shared.clone());

    let first = cache
        .get_or_load(db.default_agent_id, &agents)
        .await
        .expect("load");
    assert_eq!(first.as_str(), "test default prompt");

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

    clock.advance(Duration::from_secs(31));
    let refreshed = cache
        .get_or_load(db.default_agent_id, &agents)
        .await
        .expect("refreshed");
    assert_eq!(refreshed.as_str(), "rolled-out v2");
}

#[tokio::test(flavor = "multi_thread")]
async fn pg_memory_store_underlying_constructs() {
    // Smoke: building the store + cache via the public types matches the
    // app.rs wiring. Catches an export regression more directly than
    // the integration tests do.
    let db = TestDb::fresh().await;
    let clock: SharedClock = SystemClock::shared();
    let _store: SharedMemoryStore = Arc::new(PgMemoryStore::new(db.pool.clone(), clock.clone()));
    let _cache = SessionMemoryCache::new(4, Duration::from_secs(1), clock);
}
