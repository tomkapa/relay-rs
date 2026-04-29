//! Caps on the prompt pipeline. CLAUDE.md §5 — every value documented with *why this
//! number*; never magic numbers in logic.

use std::time::Duration;

/// Bounded `JoinSet` size for the worker pool.
///
/// Sized so a single host can sustain a reasonable steady-state concurrency without
/// exhausting the provider's per-tenant rate limit; raise via config when the
/// deployment scales horizontally.
pub const MAX_WORKERS: usize = 16;

/// Hard cap on simultaneously-pending prompts on a single session. A client storm
/// (re-tries, hot loops) cannot pin a session indefinitely; past this we refuse.
pub const MAX_PENDING_PER_SESSION: usize = 32;

/// Outer fence on a single turn (provider call + tool calls + persistence). Above this
/// the worker abandons the turn so a stuck call can never hold a lease forever.
pub const MAX_TURN_DURATION: Duration = Duration::from_secs(180);

/// Lease lifetime granted at claim time. Sized so the heartbeat cadence
/// (`LEASE_TTL / 3`) still leaves >1 chance to renew before expiry under normal load.
pub const LEASE_TTL: Duration = Duration::from_secs(30);

/// Heartbeat cadence — exactly `LEASE_TTL / 3`. Two-thirds margin tolerates a missed
/// beat before another worker claims.
pub const LEASE_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(10);

const _: () = assert!(LEASE_HEARTBEAT_INTERVAL.as_secs() * 3 == LEASE_TTL.as_secs());

/// Maximum attempts per prompt. Past this, the prompt is parked as `failed` with
/// `reason = poison` so a request that reliably crashes the worker cannot pin a
/// session forever.
pub const MAX_ATTEMPTS: u32 = 3;

/// Broadcast channel buffer per request. Slow SSE clients hitting this bound get a
/// `Stalled` chunk and must reconnect with `Last-Event-ID` to catch up via the log.
pub const MAX_CHUNK_BUFFER_PER_REQUEST: usize = 256;

/// HTTP body cap for `POST /prompts`. Crossbar with `Prompt`'s own cap; the smaller
/// applies. Belt-and-braces so the boundary check happens before deserialisation.
pub const MAX_PROMPT_BYTES: usize = 64 * 1024;

/// Soft cap on captured response bytes for replay. Past this, the persisted log is
/// truncated tail-first; the live broadcast is unaffected.
pub const MAX_RESPONSE_BYTES: usize = 1024 * 1024;

/// Maximum byte length of an idempotency key. Long enough for a UUID + version
/// prefix; short enough that a misuse cannot blow query / index size.
pub const MAX_IDEMPOTENCY_KEY_BYTES: usize = 200;

/// Polling interval used by an idle worker between `claim_next_session` calls. Flat,
/// no backoff — when notify lands later this constant disappears.
pub const WORKER_IDLE_POLL: Duration = Duration::from_secs(1);

/// Cap on the number of in-memory response slots `InMemoryResponseHub` retains.
///
/// One entry per lifetime request is unbounded by default; this ceiling forces
/// eviction (oldest closed-first, oldest live as last resort) so the process is
/// not O(lifetime requests). Sized for ~hours of steady-state replay; raise per
/// deployment if late SSE reconnect windows are longer.
pub const MAX_RESPONSE_SLOTS: usize = 4096;

/// Polling cadence used by the per-claim cancel watcher inside the worker.
///
/// Trades a single inexpensive status read every interval for mid-turn
/// cancellation latency — at this cadence a `POST /requests/:id/cancel` is
/// observed within `CANCEL_POLL_INTERVAL` and fires the agent's
/// `CancellationToken`.
pub const CANCEL_POLL_INTERVAL: Duration = Duration::from_millis(200);
