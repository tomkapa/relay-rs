//! Worker pool. A bounded `JoinSet` of N tasks, each running a claim-and-run loop:
//!
//! ```text
//! loop {
//!   match queue.claim_next_session(worker_id) {
//!     Some(claim) => {
//!       spawn heartbeat task on claim.lease (renew every TTL/3, dies on drop)
//!       result = timeout(MAX_TURN, agent.reply_batch(claim.session, prompts, cancel))
//!       publish chunks (Text, Done|Error) on the response sink
//!       mark_done | mark_failed
//!       release(lease)
//!     }
//!     None => sleep(1s)
//!   }
//! }
//! ```

use std::sync::Arc;
use std::time::Duration;

use tokio::task::{JoinHandle, JoinSet};
use tokio::time::timeout;
use tokio_util::sync::{CancellationToken, DropGuard};
use tracing::{debug, info, instrument, warn};

use async_trait::async_trait;

use crate::agent::{Agent, AgentError, SharedTurnObserver, TurnObserver};
use crate::provider::{AssistantContent, ToolResult};
use crate::types::Prompt;

use super::limits::{CANCEL_POLL_INTERVAL, MAX_TURN_DURATION, MAX_WORKERS, WORKER_IDLE_POLL};
use super::queue::{
    ClaimReceipt, ClaimedSession, LeaseTiming, SharedLeaseManager, SharedPromptQueue,
};
use super::response::{ResponseChunk, SharedResponseSink};
use super::types::{FailureReason, PromptRequestId, WorkerId};

/// Construction-time configuration for the pool.
///
/// `lease_timing` is shared with the queue: construct a single [`LeaseTiming`] and
/// pass it to both [`PgPromptQueue::with_caps`](super::pg_queue::PgPromptQueue::with_caps)
/// and this struct so the worker's heartbeat cadence is co-validated with the queue's TTL.
#[derive(Debug, Clone)]
pub struct WorkerConfig {
    pub workers: usize,
    pub lease_timing: LeaseTiming,
    pub max_turn_duration: Duration,
    pub idle_poll: Duration,
    /// Cadence at which the per-claim cancel watcher polls `queue.status`. Defaults
    /// to [`CANCEL_POLL_INTERVAL`].
    pub cancel_poll: Duration,
}

impl Default for WorkerConfig {
    fn default() -> Self {
        Self {
            workers: MAX_WORKERS,
            lease_timing: LeaseTiming::default_const(),
            max_turn_duration: MAX_TURN_DURATION,
            idle_poll: WORKER_IDLE_POLL,
            cancel_poll: CANCEL_POLL_INTERVAL,
        }
    }
}

/// Handle returned by [`WorkerPool::spawn`]. Drop or call `shutdown().await` to wind
/// down — never `tokio::spawn` a worker without holding its handle (CLAUDE.md §7).
#[derive(Debug)]
pub struct WorkerPoolHandle {
    /// Drop fires the shared cancellation token, signalling every worker to exit.
    shutdown: DropGuard,
    workers: JoinSet<()>,
}

impl WorkerPoolHandle {
    /// Signal every worker to stop and await all of them. Idempotent.
    pub async fn shutdown(mut self) {
        // Dropping the guard cancels the token; workers observe it and exit.
        drop(self.shutdown);
        while let Some(joined) = self.workers.join_next().await {
            if let Err(e) = joined {
                warn!(error = %e, "worker.join.error");
            }
        }
    }
}

#[derive(Debug)]
pub struct WorkerPool {
    queue: SharedPromptQueue,
    leases: SharedLeaseManager,
    sink: SharedResponseSink,
    agent: Agent,
    cfg: WorkerConfig,
}

impl WorkerPool {
    #[must_use]
    pub fn new(
        queue: SharedPromptQueue,
        leases: SharedLeaseManager,
        sink: SharedResponseSink,
        agent: Agent,
        cfg: WorkerConfig,
    ) -> Self {
        Self {
            queue,
            leases,
            sink,
            agent,
            cfg,
        }
    }

    /// Spawn `cfg.workers` tasks into a bounded `JoinSet` and return a handle whose
    /// drop / shutdown cleanly winds them down.
    #[must_use]
    pub fn spawn(self) -> WorkerPoolHandle {
        let mut set = JoinSet::new();
        let cfg = self.cfg.clone();
        let workers = cfg.workers.max(1);
        let shutdown = CancellationToken::new();

        for _ in 0..workers {
            let worker = Worker {
                id: WorkerId::new(),
                queue: self.queue.clone(),
                leases: self.leases.clone(),
                sink: self.sink.clone(),
                agent: self.agent.clone(),
                cfg: cfg.clone(),
                shutdown: shutdown.clone(),
            };
            set.spawn(async move { worker.run().await });
        }

        WorkerPoolHandle {
            shutdown: shutdown.drop_guard(),
            workers: set,
        }
    }
}

#[derive(Debug, Clone)]
struct Worker {
    id: WorkerId,
    queue: SharedPromptQueue,
    leases: SharedLeaseManager,
    sink: SharedResponseSink,
    agent: Agent,
    cfg: WorkerConfig,
    shutdown: CancellationToken,
}

impl Worker {
    #[instrument(name = "worker.run", skip_all, fields(relay.worker.id = %self.id))]
    async fn run(self) {
        loop {
            if self.shutdown.is_cancelled() {
                debug!("worker.shutdown");
                return;
            }
            match self.queue.claim_next_session(self.id).await {
                Ok(Some(claim)) => self.handle_claim(claim).await,
                Ok(None) => self.idle().await,
                Err(e) => {
                    warn!(error = %e, "worker.claim.error");
                    self.idle().await;
                }
            }
        }
    }

    async fn idle(&self) {
        tokio::select! {
            biased;
            () = self.shutdown.cancelled() => {},
            () = tokio::time::sleep(self.cfg.idle_poll) => {},
        }
    }

    #[instrument(
        name = "worker.handle_claim",
        skip_all,
        fields(
            relay.worker.id = %self.id,
            relay.session.id = %claim.session,
            relay.batch_size = claim.prompts.len(),
            relay.turn_seq = claim.lease.turn_seq().get(),
        ),
    )]
    async fn handle_claim(&self, claim: ClaimedSession) {
        let prompts: Vec<Prompt> = claim.prompts.iter().map(|p| p.content.clone()).collect();
        let receipt = Arc::new(claim.receipt());
        let cancel = CancellationToken::new();

        // Pre-turn cancellation check (any request flagged) — task1: "Cancellation
        // flag is checked once before the turn starts and once after it ends".
        if self.any_cancelled(receipt.ids()).await {
            self.publish_failure(receipt.ids(), &FailureReason::Cancelled)
                .await;
            self.finalise(&receipt, FailureReason::Cancelled).await;
            return;
        }

        let heartbeat = self.spawn_heartbeat(receipt.clone());
        let cancel_watcher = self.spawn_cancel_watcher(receipt.clone(), cancel.clone());
        let observer: SharedTurnObserver = Arc::new(FanOutObserver {
            sink: self.sink.clone(),
            receipt: receipt.clone(),
        });

        let outcome = timeout(
            self.cfg.max_turn_duration,
            self.agent
                .reply(claim.session, prompts, cancel.clone(), Some(observer)),
        )
        .await;

        // Stop watcher and heartbeat as soon as the turn returns; failure to abort is benign.
        cancel_watcher.abort();
        let _ = cancel_watcher.await;
        heartbeat.abort();
        let _ = heartbeat.await;

        match outcome {
            Ok(Ok(text)) => self.handle_success(&receipt, text).await,
            Ok(Err(e)) => self.handle_agent_error(&receipt, e).await,
            Err(_elapsed) => {
                warn!("worker.turn.timeout");
                self.publish_failure(receipt.ids(), &FailureReason::Timeout)
                    .await;
                self.finalise(&receipt, FailureReason::Timeout).await;
            }
        }

        if let Err(e) = self.leases.release(receipt.lease()).await {
            warn!(error = %e, "worker.lease.release.error");
        }
    }

    async fn any_cancelled(&self, ids: &[PromptRequestId]) -> bool {
        for id in ids {
            match self.queue.status(*id).await {
                Ok(view) => {
                    if view.cancellation_requested || view.status.is_terminal() {
                        return true;
                    }
                }
                Err(e) => {
                    warn!(error = %e, "worker.status.error");
                }
            }
        }
        false
    }

    async fn handle_success(&self, receipt: &ClaimReceipt, text: String) {
        info!(bytes = text.len(), "worker.turn.ok");
        // The assistant's text chunks were already streamed via the FanOutObserver
        // during the turn. The only thing left is the terminal `Done` event so SSE
        // clients (and late subscribers replaying the log) know the turn is final.
        for id in receipt.ids() {
            if let Err(e) = self
                .sink
                .publish(
                    *id,
                    ResponseChunk::Done {
                        final_text: text.clone(),
                    },
                )
                .await
            {
                warn!(error = %e, "worker.publish.done.error");
            }
            if let Err(e) = self.sink.close(*id).await {
                warn!(error = %e, "worker.sink.close.error");
            }
        }
        if let Err(e) = self.queue.mark_done(receipt).await {
            warn!(error = %e, "worker.mark_done.error");
        }
    }

    async fn handle_agent_error(&self, receipt: &ClaimReceipt, err: AgentError) {
        // Exhaustive on purpose: a new `AgentError` variant must light up here so
        // the operator decides which `FailureReason` the worker should park it as,
        // rather than silently falling through to `Provider`.
        let reason = match err {
            AgentError::Cancelled => FailureReason::Cancelled,
            AgentError::ProviderTimeout => FailureReason::Timeout,
            AgentError::HookDenied(s) => FailureReason::Hook(s),
            e @ (AgentError::Provider(_)
            | AgentError::Session(_)
            | AgentError::Memory(_)
            | AgentError::Hook(_)
            | AgentError::ToolTimeout { .. }
            | AgentError::UnknownTool(_)
            | AgentError::TooManyToolCalls { .. }
            | AgentError::MaxTurnsExceeded(_)
            | AgentError::EmptyReply) => FailureReason::Provider(e.to_string()),
        };
        warn!(reason = reason.label(), "worker.turn.error");
        self.publish_failure(receipt.ids(), &reason).await;
        self.finalise(receipt, reason).await;
    }

    async fn publish_failure(&self, ids: &[PromptRequestId], reason: &FailureReason) {
        for id in ids {
            if let Err(e) = self
                .sink
                .publish(*id, ResponseChunk::from_failure(reason))
                .await
            {
                warn!(error = %e, "worker.publish.error.error");
            }
            if let Err(e) = self.sink.close(*id).await {
                warn!(error = %e, "worker.sink.close.error");
            }
        }
    }

    async fn finalise(&self, receipt: &ClaimReceipt, reason: FailureReason) {
        if let Err(e) = self.queue.mark_failed(receipt, reason).await {
            warn!(error = %e, "worker.mark_failed.error");
        }
    }

    fn spawn_heartbeat(&self, receipt: Arc<ClaimReceipt>) -> JoinHandle<()> {
        let leases = self.leases.clone();
        let interval = self.cfg.lease_timing.heartbeat_interval();
        let shutdown = self.shutdown.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    () = shutdown.cancelled() => return,
                    () = tokio::time::sleep(interval) => {},
                }
                if let Err(e) = leases.heartbeat(receipt.lease()).await {
                    debug!(error = %e, "worker.heartbeat.stale");
                    return;
                }
            }
        })
    }

    /// Mid-turn cancellation watcher. Polls `queue.status` for every id in the
    /// receipt at `cfg.cancel_poll`; the first id observed in a cancelled or
    /// terminal state fires `cancel`, which the agent honours at its next
    /// checkpoint (between provider call and tool call). Returning [`AgentError::Cancelled`]
    /// from `agent.reply` then routes through `handle_agent_error` →
    /// `mark_failed(reason = Cancelled)`.
    fn spawn_cancel_watcher(
        &self,
        receipt: Arc<ClaimReceipt>,
        cancel: CancellationToken,
    ) -> JoinHandle<()> {
        let queue = self.queue.clone();
        let interval = self.cfg.cancel_poll;
        let shutdown = self.shutdown.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    () = shutdown.cancelled() => return,
                    () = cancel.cancelled() => return,
                    () = tokio::time::sleep(interval) => {},
                }
                for id in receipt.ids() {
                    match queue.status(*id).await {
                        Ok(view) => {
                            if view.cancellation_requested || view.status.is_terminal() {
                                debug!(relay.request.id = %id, "worker.cancel_watcher.fire");
                                cancel.cancel();
                                return;
                            }
                        }
                        Err(e) => {
                            warn!(error = %e, "worker.cancel_watcher.status.error");
                        }
                    }
                }
            }
        })
    }
}

/// Bridges `Agent` → `ResponseSink`. The agent emits per-content / per-tool-result
/// notifications via [`TurnObserver`]; this impl maps each one to a `ResponseChunk`
/// and fans it out to every `PromptRequestId` sharing the current turn.
///
/// Holds an `Arc<ClaimReceipt>` rather than a free-standing `Vec<PromptRequestId>`:
/// the receipt's ids are constructed only from a `ClaimedSession`, so the
/// "every id belongs to this turn's session" invariant is type-level (CLAUDE.md §1)
/// rather than something the worker has to remember.
#[derive(Debug)]
struct FanOutObserver {
    sink: SharedResponseSink,
    receipt: Arc<ClaimReceipt>,
}

impl FanOutObserver {
    async fn fanout(&self, chunk: ResponseChunk) {
        for id in self.receipt.ids() {
            if let Err(e) = self.sink.publish(*id, chunk.clone()).await {
                warn!(error = %e, "fanout.publish.error");
            }
        }
    }
}

#[async_trait]
impl TurnObserver for FanOutObserver {
    async fn on_assistant(&self, content: &AssistantContent) {
        let chunk = match content {
            AssistantContent::Text(s) => ResponseChunk::Text { value: s.clone() },
            AssistantContent::Reasoning(s) => ResponseChunk::Reasoning { value: s.clone() },
            AssistantContent::ToolCall(c) => ResponseChunk::ToolCall(c.clone()),
        };
        self.fanout(chunk).await;
    }

    async fn on_tool_result(&self, result: &ToolResult) {
        self.fanout(ResponseChunk::ToolResult(result.clone())).await;
    }
}

// Worker-pool tests live in `tests/runtime_pipeline.rs`, against the real Pg
// backends — see CLAUDE.md §3 (real Postgres for integration tests).
