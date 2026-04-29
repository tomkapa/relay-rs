//! Response delivery: in-memory broadcast with a persisted log per request.
//!
//! Two seams: [`ResponseSink`] (worker side — publish chunks) and [`ResponseSource`]
//! (HTTP side — subscribe). The in-memory impl maintains, per request, a `Vec` of
//! historical chunks (replay log) and a `tokio::sync::broadcast::Sender` (live tap).
//! On reconnect with `Last-Event-ID = N`, the handler reads chunks > N from the log,
//! then attaches to the broadcast.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::Stream;
use serde::Serialize;
use tokio::sync::broadcast;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::{StreamExt, wrappers::errors::BroadcastStreamRecvError};
use tracing::warn;

use crate::provider::{ToolCall, ToolResult};

use super::error::ResponseError;
use super::limits::{MAX_CHUNK_BUFFER_PER_REQUEST, MAX_RESPONSE_BYTES, MAX_RESPONSE_SLOTS};
use super::types::{ChunkSeq, FailureReason, PromptRequestId};

/// A single content chunk emitted during a turn.
///
/// `Serialize` is the wire format consumed by the SSE handler — `#[serde(tag = "kind",
/// rename_all = "snake_case")]` produces `{"kind":"text","value":"..."}` etc., and
/// [`event_kind`] returns the matching SSE `event:` name. Both come from the same
/// enum so the wire format cannot drift from the type.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ResponseChunk {
    /// Plain assistant text. Always safe to forward to a user-visible UI.
    Text { value: String },
    /// Reasoning (thinking) block. Provider-opaque; surface only to UIs that opt in
    /// since it can be PII-adjacent.
    Reasoning { value: String },
    /// Model issued a tool call. The provider's typed value is reused verbatim so
    /// the wire format cannot drift from the agent's representation.
    ToolCall(ToolCall),
    /// Tool finished. `output` is the bytes the tool returned (already capped by the
    /// agent at `TOOL_RESULT_MAX_BYTES`); `is_error` distinguishes failure from success.
    ToolResult(ToolResult),
    /// Turn completed normally. The full assistant text is included for late
    /// subscribers that don't want to reconstitute from `Text` chunks.
    Done { final_text: String },
    /// Turn failed. `reason` is the failure's `Display` form so SSE clients see
    /// provider/hook detail; tracing attributes use the low-cardinality label.
    Error { reason: String },
    /// Slow subscriber overflowed the broadcast buffer; reconnect with `Last-Event-ID`.
    Stalled,
}

impl ResponseChunk {
    #[must_use]
    pub const fn is_terminal(&self) -> bool {
        matches!(self, Self::Done { .. } | Self::Error { .. })
    }

    /// Stable, low-cardinality SSE `event:` name. Mirrors the snake_case wire tag.
    #[must_use]
    pub const fn event_kind(&self) -> &'static str {
        match self {
            Self::Text { .. } => "text",
            Self::Reasoning { .. } => "reasoning",
            Self::ToolCall(_) => "tool_call",
            Self::ToolResult(_) => "tool_result",
            Self::Done { .. } => "done",
            Self::Error { .. } => "error",
            Self::Stalled => "stalled",
        }
    }

    /// Approximate byte cost — used to enforce [`MAX_RESPONSE_BYTES`] on the persisted
    /// log without computing exact serialised size. Tool-call input is sized via
    /// `to_string()` length so a JSON object reserves roughly the right budget.
    #[must_use]
    pub fn weight(&self) -> usize {
        match self {
            Self::Text { value } | Self::Reasoning { value } => value.len(),
            Self::Error { reason } => reason.len(),
            Self::Done { final_text } => final_text.len(),
            Self::ToolCall(c) => {
                c.id.as_str().len() + c.name.as_str().len() + c.input.to_string().len()
            }
            Self::ToolResult(r) => r.call_id.as_str().len() + r.output.len(),
            Self::Stalled => 0,
        }
    }

    /// Build a wire `Error` chunk from a [`FailureReason`]. The wire payload carries
    /// the full `Display` form so SSE clients see provider/hook detail; tracing
    /// attributes use [`FailureReason::label`] for low cardinality.
    #[must_use]
    pub fn from_failure(reason: &FailureReason) -> Self {
        Self::Error {
            reason: reason.to_string(),
        }
    }
}

/// A chunk paired with its monotonic sequence number.
#[derive(Debug, Clone)]
pub struct ResponseChunkEnvelope {
    pub seq: ChunkSeq,
    pub chunk: ResponseChunk,
}

/// What an SSE stream observer sees. Wraps the broadcast lag behaviour.
#[derive(Debug, Clone)]
pub enum StreamEvent {
    Chunk(ResponseChunkEnvelope),
    /// Buffer exhausted between sends — the next attached subscriber must reconnect.
    Stalled,
}

#[async_trait]
pub trait ResponseSink: std::fmt::Debug + Send + Sync {
    async fn publish(
        &self,
        request_id: PromptRequestId,
        chunk: ResponseChunk,
    ) -> Result<ChunkSeq, ResponseError>;
    async fn close(&self, request_id: PromptRequestId) -> Result<(), ResponseError>;
}

#[async_trait]
pub trait ResponseSource: std::fmt::Debug + Send + Sync {
    /// Subscribe to a request's stream from `since` (exclusive). Replays any persisted
    /// chunks then attaches to the live broadcast. If the request is unknown, returns
    /// [`ResponseError::NotFound`].
    async fn subscribe(
        &self,
        request_id: PromptRequestId,
        since: Option<ChunkSeq>,
    ) -> Result<RequestStream, ResponseError>;
}

/// A boxed stream the SSE handler iterates. `Send` so it can move across awaits.
pub type RequestStream =
    std::pin::Pin<Box<dyn Stream<Item = Result<StreamEvent, ResponseError>> + Send>>;

/// Reference-counted publish-side handle held by workers.
pub type SharedResponseSink = Arc<dyn ResponseSink>;

/// Reference-counted subscribe-side handle held by HTTP routes.
pub type SharedResponseSource = Arc<dyn ResponseSource>;

// ============================================================================
// In-memory implementation
// ============================================================================

#[derive(Debug)]
struct StreamSlot {
    log: Vec<ResponseChunkEnvelope>,
    log_bytes: usize,
    tx: broadcast::Sender<ResponseChunkEnvelope>,
    next_seq: ChunkSeq,
    closed: bool,
}

impl StreamSlot {
    fn new() -> Self {
        let (tx, _rx) = broadcast::channel(MAX_CHUNK_BUFFER_PER_REQUEST);
        Self {
            log: Vec::new(),
            log_bytes: 0,
            tx,
            next_seq: ChunkSeq::ZERO,
            closed: false,
        }
    }
}

/// Slot table + an insertion-ordered eviction queue. Held together so an entry's
/// removal touches both atomically.
#[derive(Debug)]
struct SlotTable {
    /// Address-keyed view used by publish/subscribe.
    slots: HashMap<PromptRequestId, StreamSlot>,
    /// Insertion order. Eviction walks from the front and prefers the oldest
    /// `closed == true` slot, falling back to the oldest entry when none are closed.
    /// Same id never appears twice — every removal pulls from both maps.
    order: VecDeque<PromptRequestId>,
}

impl SlotTable {
    fn empty() -> Self {
        Self {
            slots: HashMap::new(),
            order: VecDeque::new(),
        }
    }
}

/// Process-local response delivery. Both the publish and subscribe sides share the
/// same `Mutex<SlotTable>`; `Pg*` impls drop in behind the same traits later.
#[derive(Debug)]
pub struct InMemoryResponseHub {
    table: Mutex<SlotTable>,
    log_byte_cap: usize,
    slot_cap: usize,
}

impl InMemoryResponseHub {
    #[must_use]
    pub fn new() -> Self {
        Self {
            table: Mutex::new(SlotTable::empty()),
            log_byte_cap: MAX_RESPONSE_BYTES,
            slot_cap: MAX_RESPONSE_SLOTS,
        }
    }

    #[must_use]
    pub fn with_log_cap(log_byte_cap: usize) -> Self {
        Self {
            table: Mutex::new(SlotTable::empty()),
            log_byte_cap,
            slot_cap: MAX_RESPONSE_SLOTS,
        }
    }

    /// Test-only constructor that lets a small cap be set so eviction can be
    /// exercised without staging thousands of slots.
    #[cfg(test)]
    fn with_caps(log_byte_cap: usize, slot_cap: usize) -> Self {
        assert!(slot_cap > 0, "invariant: slot_cap must be > 0");
        Self {
            table: Mutex::new(SlotTable::empty()),
            log_byte_cap,
            slot_cap,
        }
    }

    /// Lock the inner slot table. The mutex is only held across synchronous
    /// critical sections — never across `.await` — so a `std::sync::Mutex` is correct.
    fn table(&self) -> std::sync::MutexGuard<'_, SlotTable> {
        self.table
            .lock()
            .expect("invariant: response hub mutex never poisoned")
    }

    /// Touch the slot for `id`, creating it if needed. On creation, if the table is
    /// at `slot_cap` we evict — preferring the oldest closed slot, falling back to
    /// the oldest entry when none is closed. Returns a mutable handle to the slot.
    fn touch_slot(table: &mut SlotTable, id: PromptRequestId, slot_cap: usize) -> &mut StreamSlot {
        if !table.slots.contains_key(&id) {
            if table.slots.len() >= slot_cap {
                evict_one(table);
            }
            table.slots.insert(id, StreamSlot::new());
            table.order.push_back(id);
        }
        table
            .slots
            .get_mut(&id)
            .expect("invariant: slot was just inserted or already present")
    }
}

/// Evict one slot from `table`. Walks `order` from the front, prefers the first
/// closed slot; otherwise drops the oldest entry. Logs a warning when forced to
/// drop a live slot — that's a signal the cap is too low for the workload.
fn evict_one(table: &mut SlotTable) {
    // Prefer the oldest closed slot.
    let closed_idx = table
        .order
        .iter()
        .position(|id| table.slots.get(id).is_some_and(|s| s.closed));
    let victim = if let Some(idx) = closed_idx {
        table.order.remove(idx)
    } else {
        warn!(
            slots = table.slots.len(),
            "response.hub.evict_live: slot cap reached with no closed slots; dropping oldest live"
        );
        table.order.pop_front()
    };
    if let Some(id) = victim {
        table.slots.remove(&id);
    }
}

impl Default for InMemoryResponseHub {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ResponseSink for InMemoryResponseHub {
    async fn publish(
        &self,
        request_id: PromptRequestId,
        chunk: ResponseChunk,
    ) -> Result<ChunkSeq, ResponseError> {
        let mut guard = self.table();
        let slot_cap = self.slot_cap;
        let log_byte_cap = self.log_byte_cap;
        let slot = Self::touch_slot(&mut guard, request_id, slot_cap);
        if slot.closed {
            return Err(ResponseError::Backend(format!(
                "request {request_id} stream already closed"
            )));
        }
        let seq = slot.next_seq;
        slot.next_seq = seq.next()?;
        let envelope = ResponseChunkEnvelope { seq, chunk };

        slot.log_bytes = slot.log_bytes.saturating_add(envelope.chunk.weight());
        slot.log.push(envelope.clone());
        // Trim oldest entries if we cross the log cap; live subscribers are unaffected.
        while slot.log_bytes > log_byte_cap && !slot.log.is_empty() {
            let dropped = slot.log.remove(0);
            slot.log_bytes = slot.log_bytes.saturating_sub(dropped.chunk.weight());
        }

        // Broadcast send returns Err only when there are no receivers — that's fine,
        // chunks land in the log either way and a late subscriber replays from there.
        let _ = slot.tx.send(envelope);

        if matches!(slot.log.last().map(|e| &e.chunk), Some(c) if c.is_terminal()) {
            slot.closed = true;
        }

        Ok(seq)
    }

    async fn close(&self, request_id: PromptRequestId) -> Result<(), ResponseError> {
        let mut guard = self.table();
        let slot_cap = self.slot_cap;
        let slot = Self::touch_slot(&mut guard, request_id, slot_cap);
        slot.closed = true;
        Ok(())
    }
}

#[async_trait]
impl ResponseSource for InMemoryResponseHub {
    async fn subscribe(
        &self,
        request_id: PromptRequestId,
        since: Option<ChunkSeq>,
    ) -> Result<RequestStream, ResponseError> {
        let mut guard = self.table();
        let slot_cap = self.slot_cap;
        let slot = Self::touch_slot(&mut guard, request_id, slot_cap);
        // `since = None` means a fresh client (no Last-Event-ID) — replay from the
        // beginning. `since = Some(n)` resumes strictly after seq n.
        let backlog: Vec<Result<StreamEvent, ResponseError>> = slot
            .log
            .iter()
            .filter(|e| since.is_none_or(|cutoff| e.seq.get() > cutoff.get()))
            .cloned()
            .map(|e| Ok(StreamEvent::Chunk(e)))
            .collect();
        let live = BroadcastStream::new(slot.tx.subscribe());
        drop(guard);

        let backlog_stream = futures::stream::iter(backlog);

        let live_mapped = live.map(move |item| -> Result<StreamEvent, ResponseError> {
            match item {
                Ok(env) => Ok(StreamEvent::Chunk(env)),
                Err(BroadcastStreamRecvError::Lagged(_)) => Ok(StreamEvent::Stalled),
            }
        });

        // The combined stream replays backlog first, then attaches to the live tap.
        // The consumer (SSE handler / test) detects the terminal chunk itself and
        // breaks; the live tap closes naturally when the slot's broadcast sender is
        // dropped at hub teardown.
        let combined = backlog_stream.chain(live_mapped);
        Ok(Box::pin(combined))
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use futures::StreamExt;

    #[tokio::test]
    async fn publish_and_subscribe_live() {
        let hub = InMemoryResponseHub::new();
        let id = PromptRequestId::new();
        let mut stream = hub.subscribe(id, None).await.expect("subscribe");

        hub.publish(
            id,
            ResponseChunk::Text {
                value: "hello".into(),
            },
        )
        .await
        .expect("p1");
        hub.publish(
            id,
            ResponseChunk::Done {
                final_text: "hello".into(),
            },
        )
        .await
        .expect("p2");

        let mut got = Vec::new();
        while let Some(item) = tokio::time::timeout(Duration::from_millis(100), stream.next())
            .await
            .ok()
            .flatten()
        {
            got.push(item.expect("ok"));
            if matches!(
                got.last(),
                Some(StreamEvent::Chunk(e)) if e.chunk.is_terminal()
            ) {
                break;
            }
        }
        assert!(got.len() >= 2);
    }

    #[tokio::test]
    async fn replay_serves_late_subscriber() {
        let hub = InMemoryResponseHub::new();
        let id = PromptRequestId::new();
        hub.publish(id, ResponseChunk::Text { value: "a".into() })
            .await
            .expect("p1");
        hub.publish(id, ResponseChunk::Text { value: "b".into() })
            .await
            .expect("p2");
        hub.publish(
            id,
            ResponseChunk::Done {
                final_text: "ab".into(),
            },
        )
        .await
        .expect("done");

        let mut stream = hub.subscribe(id, None).await.expect("late");
        let mut got = Vec::new();
        while let Some(item) = tokio::time::timeout(Duration::from_millis(100), stream.next())
            .await
            .ok()
            .flatten()
        {
            let ev = item.expect("ok");
            let terminal = matches!(&ev, StreamEvent::Chunk(e) if e.chunk.is_terminal());
            got.push(ev);
            if terminal {
                break;
            }
        }
        assert_eq!(got.len(), 3);
    }

    #[tokio::test]
    async fn replay_respects_since_cutoff() {
        let hub = InMemoryResponseHub::new();
        let id = PromptRequestId::new();
        let s0 = hub
            .publish(id, ResponseChunk::Text { value: "a".into() })
            .await
            .expect("p1");
        hub.publish(id, ResponseChunk::Text { value: "b".into() })
            .await
            .expect("p2");
        hub.publish(
            id,
            ResponseChunk::Done {
                final_text: "ab".into(),
            },
        )
        .await
        .expect("p3");

        let mut stream = hub.subscribe(id, Some(s0)).await.expect("subscribe-since");
        let mut got = Vec::new();
        while let Some(item) = tokio::time::timeout(Duration::from_millis(100), stream.next())
            .await
            .ok()
            .flatten()
        {
            let ev = item.expect("ok");
            let terminal = matches!(&ev, StreamEvent::Chunk(e) if e.chunk.is_terminal());
            got.push(ev);
            if terminal {
                break;
            }
        }
        // Should skip s0 and replay s1 + done — i.e. 2 entries.
        assert_eq!(got.len(), 2);
    }

    #[tokio::test]
    async fn slot_cap_evicts_oldest_closed_first() {
        let hub = InMemoryResponseHub::with_caps(MAX_RESPONSE_BYTES, 2);
        let a = PromptRequestId::new();
        let b = PromptRequestId::new();
        let c = PromptRequestId::new();
        // Close `a` first (terminal chunk → closed = true).
        hub.publish(
            a,
            ResponseChunk::Done {
                final_text: "a".into(),
            },
        )
        .await
        .expect("a-done");
        hub.publish(b, ResponseChunk::Text { value: "b".into() })
            .await
            .expect("b-pub");
        // Inserting `c` should push us past cap; oldest *closed* slot (a) evicts.
        hub.publish(c, ResponseChunk::Text { value: "c".into() })
            .await
            .expect("c-pub");

        // a is gone — re-publishing into a's slot is a fresh stream (seq starts at 0).
        let seq = hub
            .publish(a, ResponseChunk::Text { value: "a2".into() })
            .await
            .expect("a-revived");
        assert_eq!(seq.get(), 0, "evicted slot's history must be discarded");
        // b still alive.
        hub.publish(b, ResponseChunk::Text { value: "b2".into() })
            .await
            .expect("b-still-alive");
    }

    #[tokio::test]
    async fn slot_cap_evicts_oldest_live_when_no_closed() {
        let hub = InMemoryResponseHub::with_caps(MAX_RESPONSE_BYTES, 2);
        let a = PromptRequestId::new();
        let b = PromptRequestId::new();
        let c = PromptRequestId::new();
        hub.publish(a, ResponseChunk::Text { value: "a".into() })
            .await
            .expect("a");
        hub.publish(b, ResponseChunk::Text { value: "b".into() })
            .await
            .expect("b");
        // No closed slot — eviction must drop the oldest live (a).
        hub.publish(c, ResponseChunk::Text { value: "c".into() })
            .await
            .expect("c");
        let seq = hub
            .publish(a, ResponseChunk::Text { value: "a2".into() })
            .await
            .expect("a-revived");
        assert_eq!(seq.get(), 0);
    }

    #[tokio::test]
    async fn log_cap_truncates_oldest() {
        let hub = InMemoryResponseHub::with_log_cap(8);
        let id = PromptRequestId::new();
        for _ in 0..10 {
            hub.publish(
                id,
                ResponseChunk::Text {
                    value: "xxxx".into(),
                },
            )
            .await
            .expect("ok");
        }
        let mut stream = hub.subscribe(id, None).await.expect("late");
        let mut got = 0;
        while let Some(item) = tokio::time::timeout(Duration::from_millis(50), stream.next())
            .await
            .ok()
            .flatten()
        {
            let _ = item.expect("ok");
            got += 1;
        }
        // Cap at 8 bytes, each chunk is 4 bytes → at most 2 chunks survive.
        assert!(got <= 3);
    }
}
