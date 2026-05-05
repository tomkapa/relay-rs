//! Postgres-backed [`ResponseSink`] / [`ResponseSource`].
//!
//! Each chunk is persisted to `prompt_response_chunks` and (if `is_terminal`) flips
//! `prompt_response_streams.closed` to TRUE. The live tap uses an in-process
//! `tokio::sync::broadcast::Sender` per request — single-process today, so SSE
//! handlers and worker tasks share the same hub. When task1.md's "binary split"
//! lands, this is where LISTEN/NOTIFY plugs in; the trait surface does not change.
//!
//! `subscribe(id, since)` subscribes to the broadcast first, then reads any backlog
//! from the chunks table. The combined stream yields the backlog then live items
//! with `seq > backlog_max` so no chunk is observed twice (a publish that lands
//! between subscribe and SELECT shows up in both, and the seq filter dedupes it).
//!
//! Wall-clock timestamps come from the injected [`SharedClock`] (CLAUDE.md §11).

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::sync::Mutex;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use futures::StreamExt;
use sqlx::PgPool;
use tokio::sync::broadcast;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::wrappers::errors::BroadcastStreamRecvError;
use tracing::warn;

use crate::clock::SharedClock;

use super::error::ResponseError;
use super::limits::MAX_CHUNK_BUFFER_PER_REQUEST;
use super::response::{
    RequestStream, ResponseChunk, ResponseChunkEnvelope, ResponseSink, ResponseSource, StreamEvent,
};
use super::types::{ChunkSeq, PromptRequestId};

/// Default cap on the in-process broadcast slot table.
///
/// Bounds memory growth across many distinct requests in a long-lived process; the
/// chunk *log* lives in Postgres, so this is purely the live-tap directory size.
/// Eviction prefers oldest closed slots (no live subscribers anyway), falling back
/// to oldest live with a warning when the cap is hit by sustained load.
pub const MAX_RESPONSE_SLOTS: usize = 4096;

#[derive(Debug)]
struct StreamSlot {
    tx: broadcast::Sender<ResponseChunkEnvelope>,
    closed: bool,
}

impl StreamSlot {
    fn new() -> Self {
        let (tx, _rx) = broadcast::channel(MAX_CHUNK_BUFFER_PER_REQUEST);
        Self { tx, closed: false }
    }
}

#[derive(Debug)]
struct SlotTable {
    slots: HashMap<PromptRequestId, StreamSlot>,
    /// Insertion order. Eviction walks from the front and prefers the oldest
    /// `closed == true` slot, falling back to the oldest entry when none are closed.
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

/// Postgres-backed publish/subscribe hub.
pub struct PgResponseHub {
    pool: PgPool,
    clock: SharedClock,
    table: Mutex<SlotTable>,
    slot_cap: usize,
}

impl PgResponseHub {
    #[must_use]
    pub fn new(pool: PgPool, clock: SharedClock) -> Self {
        Self::with_caps(pool, clock, MAX_RESPONSE_SLOTS)
    }

    #[must_use]
    pub fn with_caps(pool: PgPool, clock: SharedClock, slot_cap: usize) -> Self {
        assert!(slot_cap > 0, "invariant: slot_cap must be > 0");
        Self {
            pool,
            clock,
            table: Mutex::new(SlotTable::empty()),
            slot_cap,
        }
    }

    fn now(&self) -> DateTime<Utc> {
        DateTime::<Utc>::from(self.clock.now_wall())
    }

    fn table(&self) -> std::sync::MutexGuard<'_, SlotTable> {
        self.table
            .lock()
            .expect("invariant: response hub mutex never poisoned")
    }

    /// Subscribe to the live broadcast for `id`, creating the slot if missing.
    /// Returns `(receiver, was_already_closed)`. Held only across a sync critical
    /// section so a `std::sync::Mutex` is correct here per tokio's own guidance.
    fn subscribe_live(
        &self,
        id: PromptRequestId,
    ) -> (broadcast::Receiver<ResponseChunkEnvelope>, bool) {
        let mut guard = self.table();
        let slot_cap = self.slot_cap;
        let slot = touch_slot(&mut guard, id, slot_cap);
        (slot.tx.subscribe(), slot.closed)
    }

    /// Get or create the publish-side sender for `id`. Updates the closed flag if
    /// `terminal` is true. Returns the sender plus whether the slot was just
    /// observed as already-closed (publish into a closed slot is a backend bug).
    fn publish_sender(
        &self,
        id: PromptRequestId,
        terminal: bool,
    ) -> Result<broadcast::Sender<ResponseChunkEnvelope>, ResponseError> {
        let mut guard = self.table();
        let slot_cap = self.slot_cap;
        let slot = touch_slot(&mut guard, id, slot_cap);
        if slot.closed {
            return Err(ResponseError::Backend(format!(
                "request {id} stream already closed"
            )));
        }
        if terminal {
            slot.closed = true;
        }
        Ok(slot.tx.clone())
    }

    fn mark_closed(&self, id: PromptRequestId) {
        let mut guard = self.table();
        let slot_cap = self.slot_cap;
        let slot = touch_slot(&mut guard, id, slot_cap);
        slot.closed = true;
    }
}

impl fmt::Debug for PgResponseHub {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PgResponseHub")
            .field("slot_cap", &self.slot_cap)
            .finish_non_exhaustive()
    }
}

/// Touch the slot for `id`, creating it if needed. On creation, if the table is at
/// `slot_cap` we evict — preferring the oldest closed slot, falling back to the oldest
/// entry when none is closed.
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

fn evict_one(table: &mut SlotTable) {
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

#[async_trait]
impl ResponseSink for PgResponseHub {
    async fn publish(
        &self,
        request_id: PromptRequestId,
        chunk: ResponseChunk,
    ) -> Result<ChunkSeq, ResponseError> {
        let now = self.now();
        let terminal = chunk.is_terminal();
        let payload = serde_json::to_value(&chunk)
            .map_err(|e| ResponseError::Backend(format!("serialize chunk: {e}")))?;
        let weight_capped = chunk
            .weight()
            .min(usize::try_from(i32::MAX).expect("invariant: i32::MAX fits in usize"));
        let bytes = i32::try_from(weight_capped).expect("invariant: weight clamped to i32 range");

        let mut tx = self.pool.begin().await?;

        // Bump the per-request seq counter atomically; the row is a tiny upsert that
        // also carries the closed flag. RETURNING gives us the post-bump counter; the
        // seq we hand to this chunk is the value that was *current* before the bump,
        // so the first publish lands at seq 0 (matches ChunkSeq::ZERO and the SSE
        // handler's Last-Event-ID contract).
        let (next_seq,): (ChunkSeq,) = sqlx::query_as(
            "INSERT INTO prompt_response_streams (request_id, next_seq, closed)
             VALUES ($1, 1, $2)
             ON CONFLICT (request_id) DO UPDATE
                 SET next_seq = prompt_response_streams.next_seq + 1,
                     closed   = prompt_response_streams.closed OR EXCLUDED.closed
             RETURNING next_seq",
        )
        .bind(request_id)
        .bind(terminal)
        .fetch_one(&mut *tx)
        .await?;

        let assigned = next_seq
            .get()
            .checked_sub(1)
            .map(ChunkSeq::from)
            .expect("invariant: post-bump next_seq is at least 1");

        sqlx::query(
            "INSERT INTO prompt_response_chunks
                 (request_id, seq, payload, bytes, is_terminal, created_at)
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(request_id)
        .bind(assigned)
        .bind(&payload)
        .bind(bytes)
        .bind(terminal)
        .bind(now)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;

        let envelope = ResponseChunkEnvelope {
            seq: assigned,
            chunk: chunk.clone(),
        };

        // After DB commit, broadcast to live subscribers. Send returns Err only when
        // there are no receivers — fine, late subscribers replay from the chunks
        // table.
        let sender = self.publish_sender(request_id, terminal)?;
        let _ = sender.send(envelope);

        Ok(assigned)
    }

    async fn close(&self, request_id: PromptRequestId) -> Result<(), ResponseError> {
        sqlx::query(
            "INSERT INTO prompt_response_streams (request_id, next_seq, closed)
             VALUES ($1, 0, TRUE)
             ON CONFLICT (request_id) DO UPDATE SET closed = TRUE",
        )
        .bind(request_id)
        .execute(&self.pool)
        .await?;
        self.mark_closed(request_id);
        Ok(())
    }
}

#[async_trait]
impl ResponseSource for PgResponseHub {
    async fn subscribe(
        &self,
        request_id: PromptRequestId,
        since: Option<ChunkSeq>,
    ) -> Result<RequestStream, ResponseError> {
        // Subscribe to the live broadcast FIRST so any publish that lands between
        // this point and the backlog read is captured by both the broadcast and the
        // DB read; the seq-filter on the live stream then dedupes the overlap.
        let (live_rx, _was_closed) = self.subscribe_live(request_id);

        // Two SQL paths because the `since = None` (full replay) case has no lower
        // bound — there's no sentinel `ChunkSeq` value below `ChunkSeq::ZERO`.
        let backlog: Vec<(ChunkSeq, sqlx::types::JsonValue)> = match since {
            Some(s) => {
                sqlx::query_as(
                    "SELECT seq, payload FROM prompt_response_chunks
                     WHERE request_id = $1 AND seq > $2
                     ORDER BY seq ASC",
                )
                .bind(request_id)
                .bind(s)
                .fetch_all(&self.pool)
                .await?
            }
            None => {
                sqlx::query_as(
                    "SELECT seq, payload FROM prompt_response_chunks
                     WHERE request_id = $1
                     ORDER BY seq ASC",
                )
                .bind(request_id)
                .fetch_all(&self.pool)
                .await?
            }
        };

        let mut backlog_envelopes = Vec::with_capacity(backlog.len());
        let mut backlog_max: Option<ChunkSeq> = since;
        for (seq, payload) in backlog {
            let chunk: ResponseChunk = serde_json::from_value(payload)
                .map_err(|e| ResponseError::Backend(format!("deserialize chunk: {e}")))?;
            backlog_envelopes.push(Ok::<StreamEvent, ResponseError>(StreamEvent::Chunk(
                ResponseChunkEnvelope { seq, chunk },
            )));
            backlog_max = Some(backlog_max.map_or(seq, |m| m.max(seq)));
        }

        let backlog_stream = futures::stream::iter(backlog_envelopes);
        let dedupe_threshold = backlog_max;

        let live_mapped = BroadcastStream::new(live_rx).map(
            move |item| -> Result<Option<StreamEvent>, ResponseError> {
                match item {
                    Ok(env) => {
                        if dedupe_threshold.is_some_and(|threshold| env.seq <= threshold) {
                            // Already delivered via the backlog replay; drop.
                            Ok(None)
                        } else {
                            Ok(Some(StreamEvent::Chunk(env)))
                        }
                    }
                    Err(BroadcastStreamRecvError::Lagged(_)) => Ok(Some(StreamEvent::Stalled)),
                }
            },
        );

        // Filter out the dedupe-`None`s.
        let live_filtered = live_mapped.filter_map(|r| async move {
            match r {
                Ok(Some(ev)) => Some(Ok(ev)),
                Ok(None) => None,
                Err(e) => Some(Err(e)),
            }
        });

        Ok(Box::pin(backlog_stream.chain(live_filtered)))
    }
}
