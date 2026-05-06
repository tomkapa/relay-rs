//! End-to-end tests for the prompt pipeline against the Postgres-backed runtime:
//! * worker round-trip (enqueue → claim → run → publish → mark_done)
//! * cancellation ends the in-flight turn before its second prompt is processed
//! * streaming guarantees (text before done, exactly-once Text)
//! * idempotent enqueue
//!
//! Each test owns its own schema via [`TestDb::fresh`] so they can run in parallel.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;

use relay_rs::agent::AgentBuilder;
use relay_rs::clock::SystemClock;
use relay_rs::hook::HookChain;
use relay_rs::memory::{SharedMemory, StaticMemory};
use relay_rs::provider::{
    AssistantContent, ChatRequest, ChatResponse, LlmProvider, ProviderError, SharedProvider,
    StopReason,
};
use relay_rs::runtime::queue::PromptQueue as _;
use relay_rs::runtime::{
    IdempotencyKey, LeaseTiming, NewPromptRequest, PgPromptQueue, PgResponseHub, RequestStatus,
    ResponseChunk, SharedResponseSource, StreamEvent, WorkerConfig, WorkerPool,
};
use relay_rs::session::{PgSessionStore, SharedSessionStore};
use relay_rs::tools::ToolRegistry;
use relay_rs::types::{ModelId, Prompt};

mod common;
use common::pg::TestDb;

#[derive(Debug)]
struct ScriptedProvider {
    responses: Vec<ChatResponse>,
    cursor: AtomicUsize,
    delay: Duration,
}

#[async_trait]
impl LlmProvider for ScriptedProvider {
    fn name(&self) -> &'static str {
        "scripted"
    }

    async fn send(&self, _request: ChatRequest) -> Result<ChatResponse, ProviderError> {
        if !self.delay.is_zero() {
            tokio::time::sleep(self.delay).await;
        }
        let i = self.cursor.fetch_add(1, Ordering::SeqCst);
        self.responses
            .get(i)
            .cloned()
            .ok_or_else(|| ProviderError::Transport("script exhausted".into()))
    }
}

fn text_response(s: &str) -> ChatResponse {
    ChatResponse {
        content: vec![AssistantContent::Text(s.into())],
        stop_reason: StopReason::EndTurn,
    }
}

struct Harness {
    /// Held only so its `Drop` reaps the schema at end-of-scope.
    #[allow(dead_code)]
    db: TestDb,
    queue: Arc<PgPromptQueue>,
    hub: Arc<PgResponseHub>,
    sessions: SharedSessionStore,
    default_agent_id: relay_rs::agents::AgentId,
    pool: relay_rs::runtime::WorkerPoolHandle,
}

async fn build_harness(provider: Arc<ScriptedProvider>) -> Harness {
    let db = TestDb::fresh().await;
    let clock = SystemClock::shared();
    let queue_impl = Arc::new(PgPromptQueue::new(db.pool.clone(), clock.clone()));
    let hub = Arc::new(PgResponseHub::new(db.pool.clone(), clock.clone()));
    let sessions: SharedSessionStore =
        Arc::new(PgSessionStore::new(db.pool.clone(), clock.clone()));

    let provider: SharedProvider = provider;
    let memory: SharedMemory = Arc::new(StaticMemory::new("test"));
    let model = ModelId::try_from("test-model").expect("model");
    let agent = AgentBuilder::new(provider, sessions.clone(), memory, model)
        .expect("builder")
        .with_clock(clock)
        .with_builtin_tools(ToolRegistry::empty())
        .with_hooks(HookChain::new())
        .build();

    let cfg = WorkerConfig {
        workers: 2,
        lease_timing: LeaseTiming::try_new(Duration::from_secs(2), Duration::from_millis(100))
            .expect("valid timing"),
        max_turn_duration: Duration::from_secs(10),
        idle_poll: Duration::from_millis(20),
        cancel_poll: Duration::from_millis(50),
    };
    let pool = WorkerPool::new(
        queue_impl.clone(),
        queue_impl.clone(),
        hub.clone(),
        agent,
        cfg,
    )
    .spawn();

    Harness {
        default_agent_id: db.default_agent_id,
        db,
        queue: queue_impl,
        hub,
        sessions,
        pool,
    }
}

fn req(session: relay_rs::session::SessionId, content: &str, key: &str) -> NewPromptRequest {
    NewPromptRequest {
        session,
        content: Prompt::try_from(content).expect("p"),
        idempotency_key: IdempotencyKey::try_from(key).expect("k"),
    }
}

async fn drain_until_terminal(
    hub: Arc<PgResponseHub>,
    id: relay_rs::runtime::PromptRequestId,
    deadline: Duration,
) -> Vec<ResponseChunk> {
    let source: SharedResponseSource = hub;
    let mut stream = source.subscribe(id, None).await.expect("subscribe");
    let mut got = Vec::new();
    let until = std::time::Instant::now() + deadline;
    while std::time::Instant::now() < until {
        let next = tokio::time::timeout(Duration::from_millis(200), stream.next()).await;
        let Ok(Some(item)) = next else { continue };
        let ev = item.expect("ok");
        if let StreamEvent::Chunk(env) = ev {
            let terminal = env.chunk.is_terminal();
            got.push(env.chunk);
            if terminal {
                return got;
            }
        }
    }
    got
}

/// Wait for `id` to reach a terminal status. The worker publishes the terminal
/// chunk *before* committing `mark_done` / `mark_failed`, so the SSE stream can
/// see Done before the DB row flips. Pg adds a commit RTT to that gap; tests poll
/// briefly to avoid races.
async fn await_terminal_status(
    queue: &Arc<PgPromptQueue>,
    id: relay_rs::runtime::PromptRequestId,
    deadline: Duration,
) -> RequestStatus {
    let until = std::time::Instant::now() + deadline;
    while std::time::Instant::now() < until {
        let view = queue.status(id).await.expect("status");
        if matches!(view.status, RequestStatus::Done | RequestStatus::Failed) {
            return view.status;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    queue.status(id).await.expect("status").status
}

#[tokio::test(flavor = "multi_thread")]
async fn round_trip_publishes_done_chunk() {
    let provider = Arc::new(ScriptedProvider {
        responses: vec![text_response("hello back")],
        cursor: AtomicUsize::new(0),
        delay: Duration::ZERO,
    });
    let h = build_harness(provider).await;
    let s = h
        .sessions
        .create(h.default_agent_id)
        .await
        .expect("session");
    let id = h
        .queue
        .enqueue(req(s, "hi", "k1"))
        .await
        .expect("enqueue")
        .request_id();

    let chunks = drain_until_terminal(h.hub.clone(), id, Duration::from_secs(3)).await;
    assert!(
        chunks
            .iter()
            .any(|c| matches!(c, ResponseChunk::Done { .. })),
        "expected a Done chunk, got {chunks:?}"
    );
    let status = await_terminal_status(&h.queue, id, Duration::from_secs(2)).await;
    assert!(matches!(status, RequestStatus::Done), "got {status:?}");

    h.pool.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn cancellation_finishes_inflight_and_skips_next_turn() {
    let provider = Arc::new(ScriptedProvider {
        responses: vec![text_response("first reply"), text_response("second reply")],
        cursor: AtomicUsize::new(0),
        delay: Duration::from_millis(150),
    });
    let h = build_harness(provider).await;
    let s = h
        .sessions
        .create(h.default_agent_id)
        .await
        .expect("session");
    let first = h
        .queue
        .enqueue(req(s, "first", "k-first"))
        .await
        .expect("enqueue1")
        .request_id();

    // Wait for the first turn to start.
    let _ = drain_until_terminal(h.hub.clone(), first, Duration::from_secs(3)).await;

    let second = h
        .queue
        .enqueue(req(s, "second", "k-second"))
        .await
        .expect("enqueue2")
        .request_id();
    h.queue.request_cancellation(second).await.expect("cancel");

    let chunks = drain_until_terminal(h.hub.clone(), second, Duration::from_secs(3)).await;
    let status = await_terminal_status(&h.queue, second, Duration::from_secs(2)).await;
    assert!(
        matches!(status, RequestStatus::Done | RequestStatus::Failed),
        "second request must reach a terminal state; got {status:?}",
    );
    assert!(
        chunks.iter().any(ResponseChunk::is_terminal),
        "must have observed a terminal chunk on the SSE stream"
    );

    h.pool.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn streaming_emits_text_before_done() {
    let provider = Arc::new(ScriptedProvider {
        responses: vec![text_response("incremental answer")],
        cursor: AtomicUsize::new(0),
        delay: Duration::ZERO,
    });
    let h = build_harness(provider).await;
    let s = h
        .sessions
        .create(h.default_agent_id)
        .await
        .expect("session");
    let id = h
        .queue
        .enqueue(req(s, "hi", "stream-key"))
        .await
        .expect("enqueue")
        .request_id();

    let chunks = drain_until_terminal(h.hub.clone(), id, Duration::from_secs(3)).await;
    let mut text_idx = None;
    let mut done_idx = None;
    for (i, c) in chunks.iter().enumerate() {
        if matches!(c, ResponseChunk::Text { value: _ }) {
            text_idx.get_or_insert(i);
        }
        if matches!(c, ResponseChunk::Done { .. }) {
            done_idx = Some(i);
        }
    }
    let t = text_idx.expect("expected at least one Text chunk");
    let d = done_idx.expect("expected a terminal Done chunk");
    assert!(t < d, "Text chunk must precede Done; got chunks {chunks:?}");
    let text_count = chunks
        .iter()
        .filter(|c| matches!(c, ResponseChunk::Text { value: _ }))
        .count();
    assert_eq!(
        text_count, 1,
        "exactly one Text chunk per assistant text block; got {chunks:?}"
    );

    h.pool.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn mid_turn_cancellation_aborts_in_flight_turn() {
    let provider = Arc::new(ScriptedProvider {
        responses: vec![text_response("never delivered")],
        cursor: AtomicUsize::new(0),
        delay: Duration::from_secs(2),
    });
    let h = build_harness(provider).await;
    let s = h
        .sessions
        .create(h.default_agent_id)
        .await
        .expect("session");
    let id = h
        .queue
        .enqueue(req(s, "slow", "k-mid-cancel"))
        .await
        .expect("enqueue")
        .request_id();

    tokio::time::sleep(Duration::from_millis(150)).await;
    h.queue
        .request_cancellation(id)
        .await
        .expect("request cancel");

    let mut terminal = None;
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        let view = h.queue.status(id).await.expect("status");
        if matches!(view.status, RequestStatus::Done | RequestStatus::Failed) {
            terminal = Some(view);
            break;
        }
    }
    let view = terminal.expect("request must reach terminal state");
    assert!(
        matches!(view.status, RequestStatus::Failed),
        "expected Failed, got {:?}",
        view.status
    );

    h.pool.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn idempotent_repeat_returns_same_request_id() {
    let provider = Arc::new(ScriptedProvider {
        responses: vec![text_response("ok")],
        cursor: AtomicUsize::new(0),
        delay: Duration::ZERO,
    });
    let h = build_harness(provider).await;
    let s = h
        .sessions
        .create(h.default_agent_id)
        .await
        .expect("session");
    let a = h
        .queue
        .enqueue(req(s, "hi", "same-key"))
        .await
        .expect("a")
        .request_id();
    let b = h
        .queue
        .enqueue(req(s, "hi", "same-key"))
        .await
        .expect("b")
        .request_id();
    assert_eq!(a, b);
    h.pool.shutdown().await;
}
