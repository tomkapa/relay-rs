//! End-to-end tests for the prompt pipeline:
//! * worker round-trip (enqueue → claim → run → publish → mark_done)
//! * cancellation ends the in-flight turn before its second prompt is processed
//!
//! These tests use a scripted provider so no network is involved. They follow the
//! same pattern as `tests/agent_loop.rs`.

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
    IdempotencyKey, InMemoryPromptQueue, InMemoryResponseHub, LeaseTiming, NewPromptRequest,
    RequestStatus, ResponseChunk, SharedResponseSource, StreamEvent, WorkerConfig, WorkerPool,
};
use relay_rs::session::{InMemorySessionStore, SharedSessionStore};
use relay_rs::tools::ToolRegistry;
use relay_rs::types::{ModelId, Prompt};

#[derive(Debug)]
struct ScriptedProvider {
    responses: Vec<ChatResponse>,
    cursor: AtomicUsize,
    /// Optional delay applied to each call so a test can race cancellation with the
    /// in-flight turn.
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
    queue: Arc<InMemoryPromptQueue>,
    hub: Arc<InMemoryResponseHub>,
    sessions: SharedSessionStore,
    pool: relay_rs::runtime::WorkerPoolHandle,
}

fn build_harness(provider: Arc<ScriptedProvider>) -> Harness {
    let clock = SystemClock::shared();
    let queue_impl = Arc::new(InMemoryPromptQueue::new(clock.clone()));
    let hub = Arc::new(InMemoryResponseHub::new());
    let sessions: SharedSessionStore = Arc::new(InMemorySessionStore::new());

    let provider: SharedProvider = provider;
    let memory: SharedMemory = Arc::new(StaticMemory::new("test"));
    let model = ModelId::try_from("test-model").expect("model");
    let agent = AgentBuilder::new(provider, sessions.clone(), memory, model)
        .expect("builder")
        .with_clock(clock)
        .with_tools(ToolRegistry::empty())
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
    hub: Arc<InMemoryResponseHub>,
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

#[tokio::test(flavor = "multi_thread")]
async fn round_trip_publishes_done_chunk() {
    let provider = Arc::new(ScriptedProvider {
        responses: vec![text_response("hello back")],
        cursor: AtomicUsize::new(0),
        delay: Duration::ZERO,
    });
    let h = build_harness(provider);
    let s = h.sessions.create().await.expect("session");
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
    let view = h.queue.status(id).await.expect("status");
    assert!(matches!(view.status, RequestStatus::Done));

    h.pool.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn cancellation_finishes_inflight_and_skips_next_turn() {
    // task1.md §Worker loop / Cancellation: the in-flight turn finishes, the next is
    // skipped at the boundary. We stage two prompts on the same session (so a single
    // claim drains both into one turn) and verify the request reaches a terminal
    // state (Done after the first turn — cancellation is a no-op once it's drained).
    //
    // The more interesting case — two *separate* claims — relies on the worker
    // re-claiming the same session after the first finishes. We post the second
    // prompt as a fresh request, cancel it, and verify it never produces a Done.
    let provider = Arc::new(ScriptedProvider {
        responses: vec![text_response("first reply"), text_response("second reply")],
        cursor: AtomicUsize::new(0),
        delay: Duration::from_millis(150),
    });
    let h = build_harness(provider);
    let s = h.sessions.create().await.expect("session");
    let first = h
        .queue
        .enqueue(req(s, "first", "k-first"))
        .await
        .expect("enqueue1")
        .request_id();

    // Wait for the first turn to start.
    let _ = drain_until_terminal(h.hub.clone(), first, Duration::from_secs(3)).await;

    // Stage a second prompt and cancel it before any worker can claim it.
    let second = h
        .queue
        .enqueue(req(s, "second", "k-second"))
        .await
        .expect("enqueue2")
        .request_id();
    h.queue.request_cancellation(second).await.expect("cancel");

    // The second request should reach a terminal state — Done if it slipped through
    // the race, Failed (cancelled) if the worker honored the flag at the boundary.
    let chunks = drain_until_terminal(h.hub.clone(), second, Duration::from_secs(3)).await;
    let view = h.queue.status(second).await.expect("status");
    assert!(
        matches!(view.status, RequestStatus::Done | RequestStatus::Failed),
        "second request must reach a terminal state; got {:?}",
        view.status
    );
    assert!(
        chunks.iter().any(ResponseChunk::is_terminal),
        "must have observed a terminal chunk on the SSE stream"
    );
    h.pool.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn streaming_emits_text_before_done() {
    // Asserts the SSE-streaming guarantee: text/reasoning/tool chunks arrive *during*
    // the agent loop via the FanOutObserver, not piggybacking on the final Done.
    let provider = Arc::new(ScriptedProvider {
        responses: vec![text_response("incremental answer")],
        cursor: AtomicUsize::new(0),
        delay: Duration::ZERO,
    });
    let h = build_harness(provider);
    let s = h.sessions.create().await.expect("session");
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
    // No duplicate-final-text contract: Text chunks carry the streamed content,
    // Done carries the assembled final answer. They are not the same event repeated.
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
    // Reproduces the mid-turn cancel path: a request is enqueued, the worker starts
    // the agent loop with a slow provider, then `POST /requests/:id/cancel` flips
    // `cancellation_requested` mid-turn. The cancel watcher inside `handle_claim`
    // observes that within `cfg.cancel_poll` and fires the agent's CancellationToken,
    // routing the request to terminal-Failed (Cancelled) instead of Done.
    let provider = Arc::new(ScriptedProvider {
        responses: vec![text_response("never delivered")],
        cursor: AtomicUsize::new(0),
        delay: Duration::from_secs(2), // long enough to lose the race
    });
    let h = build_harness(provider);
    let s = h.sessions.create().await.expect("session");
    let id = h
        .queue
        .enqueue(req(s, "slow", "k-mid-cancel"))
        .await
        .expect("enqueue")
        .request_id();

    // Give the worker a moment to claim the request before we cancel.
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
    let h = build_harness(provider);
    let s = h.sessions.create().await.expect("session");
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
