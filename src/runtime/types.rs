//! Domain primitives for the prompt pipeline. CLAUDE.md §1 — every value carrying an
//! invariant is wrapped in a newtype with a `TryFrom` smart constructor.

use std::fmt;

use serde::Serialize;
use thiserror::Error;
use uuid::Uuid;

use crate::types::ParseError;

use super::limits::{MAX_ATTEMPTS, MAX_IDEMPOTENCY_KEY_BYTES};

/// Sequence number stepped past `u64::MAX`. Astronomical, but typed so callers know
/// what failed without parsing a string.
#[derive(Debug, Clone, Copy, Error)]
#[error("{seq} overflow")]
pub struct SeqOverflow {
    /// Name of the sequence that overflowed (`"turn_seq"`, `"chunk_seq"`).
    pub seq: &'static str,
}

/// Opaque request id — minted by the queue when a prompt is enqueued.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct PromptRequestId(Uuid);

impl PromptRequestId {
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    #[must_use]
    pub fn as_uuid(self) -> Uuid {
        self.0
    }

    /// Reconstruct an id from its wire form. As with `SessionId`, the UUID itself
    /// carries no domain invariant — existence is checked by the queue.
    #[must_use]
    pub const fn from_uuid(raw: Uuid) -> Self {
        Self(raw)
    }
}

impl Default for PromptRequestId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for PromptRequestId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("PromptRequestId").field(&self.0).finish()
    }
}

impl fmt::Display for PromptRequestId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl TryFrom<&str> for PromptRequestId {
    type Error = ParseError;
    fn try_from(raw: &str) -> Result<Self, Self::Error> {
        Uuid::parse_str(raw)
            .map(Self)
            .map_err(|_| ParseError::Malformed {
                field: "request_id",
                detail: "not a UUID",
            })
    }
}

/// Identifier for an individual worker in the pool. Carried on lease tokens so we can
/// trace which worker held a session.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct WorkerId(Uuid);

impl WorkerId {
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    #[must_use]
    pub fn as_uuid(self) -> Uuid {
        self.0
    }
}

impl Default for WorkerId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for WorkerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("WorkerId").field(&self.0).finish()
    }
}

impl fmt::Display for WorkerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// Monotonically-increasing sequence number bumped on every claim. Stale writes from
/// a zombie worker are rejected when the token's seq does not match the current row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TurnSeq(u64);

impl TurnSeq {
    pub const ZERO: Self = Self(0);

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    /// Step to the next sequence. Returns [`SeqOverflow`] on overflow — astronomical,
    /// but typed per CLAUDE.md §12.
    pub const fn next(self) -> Result<Self, SeqOverflow> {
        match self.0.checked_add(1) {
            Some(n) => Ok(Self(n)),
            None => Err(SeqOverflow { seq: "turn_seq" }),
        }
    }
}

impl fmt::Display for TurnSeq {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// Number of attempts a request has been claimed. Capped at [`MAX_ATTEMPTS`]; one over
/// the cap means the row is parked with reason = poison.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Attempts(u32);

impl Attempts {
    pub const ZERO: Self = Self(0);

    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }

    /// Bump and return whether the new value is still below the poison threshold.
    pub fn increment(&mut self) -> AttemptOutcome {
        self.0 = self.0.saturating_add(1);
        if self.0 >= MAX_ATTEMPTS {
            AttemptOutcome::Poisoned
        } else {
            AttemptOutcome::Live
        }
    }
}

/// Outcome of an [`Attempts::increment`] call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttemptOutcome {
    Live,
    Poisoned,
}

/// A free-form, client-supplied idempotency key. Bounded length so the dedup index
/// (today: a `HashMap`; tomorrow: a Postgres unique index) cannot grow unboundedly.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct IdempotencyKey(String);

impl IdempotencyKey {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for IdempotencyKey {
    type Error = ParseError;
    fn try_from(raw: String) -> Result<Self, Self::Error> {
        if raw.is_empty() {
            return Err(ParseError::Empty {
                field: "idempotency_key",
            });
        }
        if raw.len() > MAX_IDEMPOTENCY_KEY_BYTES {
            return Err(ParseError::TooLong {
                field: "idempotency_key",
                max: MAX_IDEMPOTENCY_KEY_BYTES,
                got: raw.len(),
            });
        }
        Ok(Self(raw))
    }
}

impl TryFrom<&str> for IdempotencyKey {
    type Error = ParseError;
    fn try_from(raw: &str) -> Result<Self, Self::Error> {
        Self::try_from(raw.to_string())
    }
}

/// Lifecycle state of a prompt request row. The pipeline is forward-only:
/// `Pending -> Processing -> Done | Failed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestStatus {
    Pending,
    Processing,
    Done,
    Failed,
}

/// Why a request was marked failed. Closed enum so every caller of `mark_failed` is
/// exhaustive at compile time.
#[derive(Debug, Clone)]
pub enum FailureReason {
    Cancelled,
    Timeout,
    Provider(String),
    Hook(String),
    /// Hit `MAX_ATTEMPTS` without ever succeeding.
    Poison,
    /// A precondition the worker can never recover from (e.g. session vanished).
    Unrecoverable(String),
}

impl FailureReason {
    /// Stable, low-cardinality label for tracing attributes.
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match self {
            Self::Cancelled => "cancelled",
            Self::Timeout => "timeout",
            Self::Provider(_) => "provider",
            Self::Hook(_) => "hook",
            Self::Poison => "poison",
            Self::Unrecoverable(_) => "unrecoverable",
        }
    }
}

impl fmt::Display for FailureReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => f.write_str("cancelled"),
            Self::Timeout => f.write_str("turn timed out"),
            Self::Provider(s) => write!(f, "provider error: {s}"),
            Self::Hook(s) => write!(f, "hook denied: {s}"),
            Self::Poison => f.write_str("max attempts exceeded"),
            Self::Unrecoverable(s) => write!(f, "unrecoverable: {s}"),
        }
    }
}

/// Sequence number for an individual chunk on a request stream. Strictly monotonic
/// per request so an SSE client reconnecting can pass `Last-Event-ID` to skip already-
/// observed chunks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ChunkSeq(u64);

impl ChunkSeq {
    pub const ZERO: Self = Self(0);

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    /// Step to the next sequence. Returns [`SeqOverflow`] on overflow.
    pub const fn next(self) -> Result<Self, SeqOverflow> {
        match self.0.checked_add(1) {
            Some(n) => Ok(Self(n)),
            None => Err(SeqOverflow { seq: "chunk_seq" }),
        }
    }
}

impl From<u64> for ChunkSeq {
    fn from(n: u64) -> Self {
        Self(n)
    }
}

impl fmt::Display for ChunkSeq {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idempotency_key_rejects_empty_and_oversize() {
        assert!(IdempotencyKey::try_from("").is_err());
        assert!(IdempotencyKey::try_from("a".repeat(MAX_IDEMPOTENCY_KEY_BYTES + 1)).is_err());
        let k = IdempotencyKey::try_from("ok").expect("valid");
        assert_eq!(k.as_str(), "ok");
    }

    #[test]
    fn turn_seq_advances_monotonically() {
        let s = TurnSeq::ZERO;
        let s1 = s.next().expect("under cap");
        assert!(s1 > s);
    }

    #[test]
    fn attempts_poisons_at_cap() {
        let mut a = Attempts::ZERO;
        for _ in 0..(MAX_ATTEMPTS - 1) {
            assert_eq!(a.increment(), AttemptOutcome::Live);
        }
        assert_eq!(a.increment(), AttemptOutcome::Poisoned);
    }
}
