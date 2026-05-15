//! Integration tests for the chat-thread HTTP routes (G1, G2, G3).
//!
//! Boots an `AppState` against a fresh schema-per-test pool, builds the axum
//! `router` with the same wiring as production, and pokes it via
//! `tower::ServiceExt::oneshot`. Assertions cover:
//!
//! - G1: the channel feed lists every human-rooted DAG.
//! - G2: the flat thread history is empty for a fresh enqueue with no
//!   appended `session_messages`.
//! - G3 (NOTIFY): a chunk published via [`PgResponseHub`] arrives on the
//!   live thread stream subscriber for the matching root.

#![allow(clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use relay_rs::agents::SharedAgentStore;
use relay_rs::clock::{SharedClock, SystemClock};
use relay_rs::http::{AppState, router};
use relay_rs::mcp::{McpRefresher, McpRegistry, PgMcpServerStore, SharedMcpServerStore};
use relay_rs::provider::{ChatMessage, UserContent};
use relay_rs::runtime::{
    IdempotencyKey, NewPromptRequest, PgDagBudget, PgPromptQueue, PgResponseHub, PgThreadStream,
    PromptRequestId, ResponseChunk, SharedDagBudget, SharedLeaseManager, SharedPromptQueue,
    SharedResponseSink, SharedResponseSource, SharedThreadStream, ThreadStreamEvent,
};
use relay_rs::session::{PgSessionStore, SharedSessionStore};
use relay_rs::types::{MessageSender, Participant, Prompt};
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;

mod common;
use common::pg::TestDb;

/// Minimal HTTP harness wired with every collaborator the threads routes
/// touch, plus the live `PgThreadStream` so G3 NOTIFY round-trips through
/// the real listener task.
struct ThreadsHarness {
    db: TestDb,
    queue: SharedPromptQueue,
    sink: SharedResponseSink,
    thread_stream: SharedThreadStream,
    state: AppState,
    /// Held so its `Drop` reaps the coordinator task.
    #[allow(dead_code)]
    refresher: McpRefresher,
}

impl ThreadsHarness {
    async fn new() -> Self {
        let db = TestDb::fresh().await;
        let clock: SharedClock = SystemClock::shared();
        let pool: PgPool = db.pool.clone();

        let queue_impl = Arc::new(PgPromptQueue::new(pool.clone(), clock.clone()));
        let queue: SharedPromptQueue = queue_impl.clone();
        let leases: SharedLeaseManager = queue_impl;

        let hub = Arc::new(PgResponseHub::new(pool.clone(), clock.clone()));
        let sink: SharedResponseSink = hub.clone();
        let responses: SharedResponseSource = hub;

        let sessions: SharedSessionStore =
            Arc::new(PgSessionStore::new(pool.clone(), clock.clone()));
        let agent_store: SharedAgentStore =
            common::pg::shared_agent_store(pool.clone(), clock.clone());
        let dag: SharedDagBudget = Arc::new(PgDagBudget::new(pool.clone()));

        let mcp_store: SharedMcpServerStore =
            Arc::new(PgMcpServerStore::new(pool.clone(), clock.clone()));
        let mcp_registry = McpRegistry::new(mcp_store.clone(), clock.clone());
        let (refresher, mcp_refresh) = McpRefresher::spawn(mcp_registry);

        let thread_stream: SharedThreadStream =
            PgThreadStream::spawn(pool.clone(), CancellationToken::new())
                .await
                .expect("spawn thread stream");

        let memory_store: relay_rs::memory::SharedMemoryStore =
            Arc::new(relay_rs::memory::PgMemoryStore::new(
                pool.clone(),
                clock.clone(),
                common::embedding::FakeEmbeddingProvider::shared(),
            ));
        let state = AppState {
            queue: queue.clone(),
            leases,
            responses,
            sessions,
            agents: agent_store,
            dag,
            memory_store,
            mcp_store,
            mcp_refresh,
            thread_stream: thread_stream.clone(),
            pool,
        };

        Self {
            db,
            queue,
            sink,
            thread_stream,
            state,
            refresher,
        }
    }
}

async fn enqueue_human_root(harness: &ThreadsHarness, content: &str, key: &str) -> PromptRequestId {
    harness
        .queue
        .enqueue(NewPromptRequest {
            session: None,
            sender: Participant::Human,
            receiver_agent_id: harness.db.default_agent_id,
            parent_session: None,
            content: Prompt::try_from(content).expect("prompt"),
            idempotency_key: IdempotencyKey::try_from(key).expect("key"),
            kind_payload: relay_rs::runtime::RequestKindPayload::Normal {},
        })
        .await
        .expect("enqueue")
        .request_id()
}

#[tokio::test(flavor = "multi_thread")]
async fn list_threads_returns_one_row_per_human_root() {
    let h = ThreadsHarness::new().await;
    let r1 = enqueue_human_root(&h, "first", "k-1").await;
    let r2 = enqueue_human_root(&h, "second", "k-2").await;

    let app = router(h.state.clone());
    let res = app
        .oneshot(
            axum::http::Request::builder()
                .uri("/threads")
                .body(axum::body::Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(res.status(), axum::http::StatusCode::OK);

    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .expect("collect");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("json");
    let rows = json.as_array().expect("array");
    assert_eq!(rows.len(), 2, "two human roots → two thread rows");

    let ids: Vec<String> = rows
        .iter()
        .map(|r| r["root_request_id"].as_str().expect("uuid").to_string())
        .collect();
    assert!(ids.contains(&r1.to_string()));
    assert!(ids.contains(&r2.to_string()));

    for r in rows {
        assert_eq!(r["reply_count"].as_i64(), Some(0));
        assert_eq!(r["status"].as_str(), Some("pending"));
        assert!(r["first_agent"]["name"].is_string());
        assert!(r["preview"].is_string());
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn thread_messages_returns_empty_for_fresh_dag() {
    let h = ThreadsHarness::new().await;
    let root = enqueue_human_root(&h, "hello", "k-1").await;

    let app = router(h.state.clone());
    let res = app
        .oneshot(
            axum::http::Request::builder()
                .uri(format!("/threads/{}/messages", root.as_uuid()))
                .body(axum::body::Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(res.status(), axum::http::StatusCode::OK);

    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .expect("collect");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(
        json.as_array().expect("array").len(),
        0,
        "fresh enqueue has no session_messages",
    );
}

/// Every appended `session_messages` row carries the `request_id` that
/// produced it, and the G2 read endpoint surfaces it on the wire so the FE
/// can dedupe optimistic / live / persisted bubbles by identity (no text
/// matching). See `doc/thread_panel_refactor_export.md` for the rationale.
#[tokio::test(flavor = "multi_thread")]
async fn thread_messages_includes_request_id_per_row() {
    let h = ThreadsHarness::new().await;
    let root = enqueue_human_root(&h, "hello", "k-1").await;

    // Stand up the human↔agent session bound to the enqueued DAG root, then
    // append one human turn carrying the same request_id the queue minted.
    let agent = Participant::agent(h.db.default_agent_id);
    let session = h
        .state
        .sessions
        .resolve_or_create_for_pair(root, Participant::Human, agent, None)
        .await
        .expect("create session");
    h.state
        .sessions
        .append(
            session,
            MessageSender::Human,
            agent,
            ChatMessage::User(vec![UserContent::Text("hello".into())]),
            root,
        )
        .await
        .expect("append");

    let app = router(h.state.clone());
    let res = app
        .oneshot(
            axum::http::Request::builder()
                .uri(format!("/threads/{}/messages", root.as_uuid()))
                .body(axum::body::Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(res.status(), axum::http::StatusCode::OK);

    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .expect("collect");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("json");
    let rows = json.as_array().expect("array");
    assert_eq!(rows.len(), 1, "one appended row → one history row");
    assert_eq!(
        rows[0]["request_id"].as_str(),
        Some(root.as_uuid().to_string().as_str()),
        "history row must carry the request_id that produced it",
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn notify_drives_thread_stream_subscriber() {
    let h = ThreadsHarness::new().await;
    let root = enqueue_human_root(&h, "hi", "k-1").await;

    // Subscribe to the live fan-in stream BEFORE publishing so the slot is
    // attached and `handle_notification` doesn't drop the chunk.
    let mut stream = h.thread_stream.subscribe(root);

    h.sink
        .publish(
            root,
            ResponseChunk::Text {
                value: "hello human".into(),
            },
        )
        .await
        .expect("publish");

    let item = tokio::time::timeout(Duration::from_secs(2), stream.next())
        .await
        .expect("notification arrived")
        .expect("stream item")
        .expect("ok");

    match item {
        ThreadStreamEvent::Item(ev) => {
            assert_eq!(ev.request_id, root);
            assert!(matches!(ev.chunk, ResponseChunk::Text { .. }));
            assert_eq!(ev.from_agent, h.db.default_agent_id);
        }
        ThreadStreamEvent::Stalled => panic!("unexpected stalled event"),
    }
}
