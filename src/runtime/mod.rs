//! Agent runtime.
//!
//! The execution layer the HTTP boundary calls into: request queue, lease management,
//! worker pool, and response (SSE) delivery. Traits are the seam — the in-memory
//! impls land today, the Postgres impls drop in behind the same traits when storage
//! moves. See `doc/task1.md` for the full design.

pub mod error;
pub mod limits;
pub mod queue;
pub mod response;
pub mod types;
pub mod worker;

pub use error::{LeaseTimingError, PromptError, ResponseError};
pub use limits::{
    CANCEL_POLL_INTERVAL, LEASE_HEARTBEAT_INTERVAL, LEASE_TTL, MAX_ATTEMPTS,
    MAX_CHUNK_BUFFER_PER_REQUEST, MAX_PENDING_PER_SESSION, MAX_PROMPT_BYTES, MAX_RESPONSE_SLOTS,
    MAX_TURN_DURATION, MAX_WORKERS,
};
pub use queue::{
    ClaimReceipt, ClaimedPrompt, ClaimedSession, EnqueueOutcome, InMemoryPromptQueue, LeaseManager,
    LeaseTiming, LeaseToken, NewPromptRequest, PromptQueue, RequestStatusView, SharedLeaseManager,
    SharedPromptQueue,
};
pub use response::{
    InMemoryResponseHub, ResponseChunk, ResponseChunkEnvelope, ResponseSink, ResponseSource,
    SharedResponseSink, SharedResponseSource, StreamEvent,
};
pub use types::{
    Attempts, ChunkSeq, FailureReason, IdempotencyKey, PromptRequestId, RequestStatus, SeqOverflow,
    TurnSeq, WorkerId,
};
pub use worker::{WorkerConfig, WorkerPool, WorkerPoolHandle};
