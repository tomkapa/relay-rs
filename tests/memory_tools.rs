//! Trait-contract tests for the Phase 3 memory mutation tools
//! (doc/memory.md §1.5, §2.3): `memory_write`, `memory_update`,
//! `memory_forget`. The tools share a per-turn mutation counter so a
//! single turn cannot exceed [`MAX_MEMORY_MUTATIONS_PER_TURN`].
//!
//! `recall` is intentionally absent — it depends on the embedding
//! provider that lands in Phase 9.

#![allow(clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use relay_rs::agents::{AgentPromptCache, PgAgentStore, SharedAgentStore};
use relay_rs::clock::{SharedClock, SystemClock};
use relay_rs::memory::{
    AgentMemory, MAX_MEMORY_MUTATIONS_PER_TURN, MemoryContent, MemoryHandle, MemoryKind,
    MemoryMutation, MemoryState, MutationSource, PgMemoryStore, SessionMemoryCache,
    SharedMemoryStore,
};
use relay_rs::runtime::PromptRequestId;
use relay_rs::session::{PgSessionStore, SharedSessionStore};
use relay_rs::tools::system::{
    MemoryForgetTool, MemoryToolDeps, MemoryUpdateTool, MemoryWriteTool,
};
use relay_rs::tools::{Tool, ToolCallContext, ToolError};
use relay_rs::types::Participant;
use serde_json::json;

mod common;
use common::pg::{TestDb, human_to_agent_session, seed_prompt_request};

struct Fixture {
    deps: MemoryToolDeps,
    store: SharedMemoryStore,
    session: relay_rs::session::SessionId,
    agent_id: relay_rs::agents::AgentId,
}

async fn fixture(db: &TestDb) -> Fixture {
    let clock: SharedClock = SystemClock::shared();
    let agents: SharedAgentStore = Arc::new(PgAgentStore::new(db.pool.clone(), clock.clone()));
    let sessions: SharedSessionStore =
        Arc::new(PgSessionStore::new(db.pool.clone(), clock.clone()));
    let prompt_cache = Arc::new(AgentPromptCache::new(
        8,
        Duration::from_secs(60),
        clock.clone(),
    ));
    let store: SharedMemoryStore = Arc::new(PgMemoryStore::new(db.pool.clone(), clock.clone()));
    let session_cache = Arc::new(SessionMemoryCache::new(8, Duration::from_secs(60), clock));
    let _memory = AgentMemory::new(
        agents,
        prompt_cache,
        store.clone(),
        session_cache.clone(),
        "core",
    );
    let session = human_to_agent_session(sessions.as_ref(), db.default_agent_id).await;
    Fixture {
        deps: MemoryToolDeps::new(store.clone(), session_cache),
        store,
        session,
        agent_id: db.default_agent_id,
    }
}

fn ctx(f: &Fixture, request_id: PromptRequestId) -> ToolCallContext {
    ToolCallContext {
        session_id: f.session,
        viewer: Participant::agent(f.agent_id),
        root_request_id: request_id,
        request_id,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn memory_write_creates_tentative_row() {
    let db = TestDb::fresh().await;
    let f = fixture(&db).await;
    let tool = MemoryWriteTool::new(f.deps.clone());
    let request = seed_prompt_request(&db.pool, f.session, f.agent_id).await;

    let out = tool
        .execute_with_ctx(
            json!({"kind": "self", "content": "I default to terse replies."}),
            &ctx(&f, request),
        )
        .await
        .expect("write");
    let parsed: serde_json::Value = serde_json::from_str(&out).expect("json");
    assert_eq!(parsed["state"], "tentative");
    assert_eq!(parsed["kind"], "self");

    let listed = f.store.list(f.agent_id).await.expect("list");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].state, MemoryState::Tentative);
    assert_eq!(listed[0].kind, MemoryKind::Identity);
    assert!(!listed[0].pinned);
}

#[tokio::test(flavor = "multi_thread")]
async fn memory_update_resolves_handle_and_resets_state() {
    let db = TestDb::fresh().await;
    let f = fixture(&db).await;

    // Seed a memory at Held; the update tool should bring it back to
    // Tentative because the content changed.
    let written = f
        .store
        .apply(MemoryMutation::Write {
            agent: f.agent_id,
            kind: MemoryKind::Identity,
            content: MemoryContent::try_from("original").expect("c"),
            state: MemoryState::Held,
            pinned: false,
            source: MutationSource::Operator,
        })
        .await
        .expect("seed");

    // Compose the section so the handle map is populated. We do this
    // through the same path the tool uses: a `resolve_handle` call.
    let request = seed_prompt_request(&db.pool, f.session, f.agent_id).await;
    let _ = MemoryWriteTool::new(f.deps.clone()); // ensure deps usable
    let update = MemoryUpdateTool::new(f.deps.clone());
    // The tool resolves M-1 via the session cache it shares.
    let out = update
        .execute_with_ctx(
            json!({"handle": "M-1", "content": "revised"}),
            &ctx(&f, request),
        )
        .await
        .expect("update");
    let parsed: serde_json::Value = serde_json::from_str(&out).expect("json");
    assert_eq!(parsed["memory_id"], written.memory_id.as_uuid().to_string());
    assert_eq!(parsed["state"], "tentative");

    let row = f
        .store
        .get(written.memory_id)
        .await
        .expect("get")
        .expect("present");
    assert_eq!(row.content.as_str(), "revised");
    assert_eq!(row.state, MemoryState::Tentative);
}

#[tokio::test(flavor = "multi_thread")]
async fn memory_update_pinned_row_rejects_agent_call() {
    let db = TestDb::fresh().await;
    let f = fixture(&db).await;
    let _written = f
        .store
        .apply(MemoryMutation::Write {
            agent: f.agent_id,
            kind: MemoryKind::Identity,
            content: MemoryContent::try_from("pinned").expect("c"),
            state: MemoryState::Core,
            pinned: true,
            source: MutationSource::Operator,
        })
        .await
        .expect("seed pinned");

    let update = MemoryUpdateTool::new(f.deps.clone());
    let request = seed_prompt_request(&db.pool, f.session, f.agent_id).await;
    let err = update
        .execute_with_ctx(
            json!({"handle": "M-1", "content": "agent attempt"}),
            &ctx(&f, request),
        )
        .await
        .expect_err("agent edit blocked");
    assert!(matches!(err, ToolError::InvalidInput(_)), "got {err:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn memory_forget_removes_row() {
    let db = TestDb::fresh().await;
    let f = fixture(&db).await;
    let written = f
        .store
        .apply(MemoryMutation::Write {
            agent: f.agent_id,
            kind: MemoryKind::Identity,
            content: MemoryContent::try_from("disposable").expect("c"),
            state: MemoryState::Held,
            pinned: false,
            source: MutationSource::Operator,
        })
        .await
        .expect("seed");

    let forget = MemoryForgetTool::new(f.deps.clone());
    let request = seed_prompt_request(&db.pool, f.session, f.agent_id).await;
    let out = forget
        .execute_with_ctx(json!({"handle": "M-1"}), &ctx(&f, request))
        .await
        .expect("forget");
    let parsed: serde_json::Value = serde_json::from_str(&out).expect("json");
    assert_eq!(parsed["status"], "forgotten");

    assert!(
        f.store.get(written.memory_id).await.expect("get").is_none(),
        "row should be gone"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn unknown_handle_surfaces_invalid_input() {
    let db = TestDb::fresh().await;
    let f = fixture(&db).await;
    let update = MemoryUpdateTool::new(f.deps.clone());
    let request = seed_prompt_request(&db.pool, f.session, f.agent_id).await;
    let err = update
        .execute_with_ctx(
            json!({"handle": "M-99", "content": "nope"}),
            &ctx(&f, request),
        )
        .await
        .expect_err("should fail");
    assert!(matches!(err, ToolError::InvalidInput(_)), "got {err:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn per_turn_cap_blocks_overflow_within_one_request_id() {
    let db = TestDb::fresh().await;
    let f = fixture(&db).await;
    let write = MemoryWriteTool::new(f.deps.clone());
    let request = seed_prompt_request(&db.pool, f.session, f.agent_id).await;

    // The first MAX_MEMORY_MUTATIONS_PER_TURN writes succeed.
    for i in 0..MAX_MEMORY_MUTATIONS_PER_TURN {
        write
            .execute_with_ctx(
                json!({"kind": "self", "content": format!("memory {i}")}),
                &ctx(&f, request),
            )
            .await
            .expect("under cap");
    }
    let err = write
        .execute_with_ctx(
            json!({"kind": "self", "content": "one too many"}),
            &ctx(&f, request),
        )
        .await
        .expect_err("over cap");
    assert!(
        matches!(&err, ToolError::InvalidInput(s) if s.contains("cap")),
        "got {err:?}"
    );

    // A different request id has its own quota.
    let new_req = seed_prompt_request(&db.pool, f.session, f.agent_id).await;
    write
        .execute_with_ctx(
            json!({"kind": "self", "content": "fresh turn"}),
            &ctx(&f, new_req),
        )
        .await
        .expect("fresh turn ok");
}

#[tokio::test(flavor = "multi_thread")]
async fn handle_round_trips_through_session_cache() {
    let db = TestDb::fresh().await;
    let f = fixture(&db).await;
    let store = f.store.clone();
    let written = store
        .apply(MemoryMutation::Write {
            agent: f.agent_id,
            kind: MemoryKind::Identity,
            content: MemoryContent::try_from("first").expect("c"),
            state: MemoryState::Held,
            pinned: false,
            source: MutationSource::Operator,
        })
        .await
        .expect("seed");

    // Resolve via the same session-cache the tools use; M-1 should map
    // to the seeded memory id.
    let agents: SharedAgentStore =
        Arc::new(PgAgentStore::new(db.pool.clone(), SystemClock::shared()));
    let memory = AgentMemory::new(
        agents,
        Arc::new(AgentPromptCache::new(
            2,
            Duration::from_secs(60),
            SystemClock::shared(),
        )),
        store,
        f.deps.session_cache.clone(),
        "core",
    );
    let resolved = memory
        .resolve_handle(
            f.session,
            f.agent_id,
            MemoryHandle::try_from(1u32).expect("h"),
        )
        .await
        .expect("resolve");
    assert_eq!(resolved, Some(written.memory_id));
}
