//! Composition root.
//!
//! Wires every trait object the agent and runtime need. Each piece is constructed
//! once at startup; adding a new tool, swapping the queue backend for Postgres, or
//! chaining a policy hook is a one-line change here — the agent and runtime
//! themselves do not move.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use reqwest::Client;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::agent::{Agent, AgentBuilder};
use crate::clock::{SharedClock, SystemClock};
use crate::config::Settings;
use crate::error::AppError;
use crate::hook::HookChain;
use crate::http::{AppState, router};
use crate::memory::{SharedMemory, StaticMemory};
use crate::provider::SharedProvider;
use crate::provider::anthropic::AnthropicProvider;
use crate::runtime::{
    InMemoryPromptQueue, InMemoryResponseHub, SharedLeaseManager, SharedPromptQueue,
    SharedResponseSink, SharedResponseSource, WorkerConfig, WorkerPool, WorkerPoolHandle,
};
use crate::session::{InMemorySessionStore, SharedSessionStore};
use crate::tools::{ToolRegistry, WebFetchTool, WebSearchTool};

const HTTP_USER_AGENT: &str = concat!("relay-rs/", env!("CARGO_PKG_VERSION"));
const HTTP_DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
const HTTP_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

const DEFAULT_SYSTEM_PROMPT: &str = "You are Relay, a helpful AI agent. \
    You are concise, accurate, and prefer to verify facts using your tools \
    before answering when the answer is not obvious. \
    When you call a tool, briefly state why before the call. \
    When you have enough information, give the user a clear final answer.";

/// All the pieces a deployment needs to serve HTTP + run workers in-process.
#[derive(Debug)]
pub struct Server {
    pub state: AppState,
    pub workers: WorkerPoolHandle,
    pub http_addr: SocketAddr,
}

/// Pre-built collaborators shared by the agent and the runtime. Constructed once at
/// startup and consumed by [`build_agent_from`] / [`build_server`].
#[derive(Debug)]
struct Collaborators {
    provider: SharedProvider,
    sessions: SharedSessionStore,
    memory: SharedMemory,
    clock: SharedClock,
    tools: ToolRegistry,
}

impl Collaborators {
    fn new(settings: &Settings) -> Result<Self, AppError> {
        let http = build_http_client()?;
        Ok(Self {
            provider: build_provider(settings)?,
            sessions: Arc::new(InMemorySessionStore::new()),
            memory: Arc::new(StaticMemory::new(DEFAULT_SYSTEM_PROMPT)),
            clock: SystemClock::shared(),
            tools: build_tools(settings, http)?,
        })
    }
}

/// Build a fully-wired [`Agent`] from configuration. Kept for tests and any consumer
/// that wants the agent without the HTTP/worker stack.
pub fn build_agent(settings: Settings) -> Result<Agent, AppError> {
    let pieces = Collaborators::new(&settings)?;
    Ok(build_agent_from(&pieces, &settings))
}

fn build_agent_from(pieces: &Collaborators, settings: &Settings) -> Agent {
    AgentBuilder::new(
        pieces.provider.clone(),
        pieces.sessions.clone(),
        pieces.memory.clone(),
        settings.model.clone(),
    )
    .expect("invariant: limits constants are static and parse")
    .with_tools(pieces.tools.clone())
    .with_hooks(HookChain::new())
    .with_clock(pieces.clock.clone())
    .build()
}

/// Build the full HTTP + worker pool composition. The returned [`Server`] is ready to
/// hand to `axum::serve` and a graceful-shutdown loop.
pub fn build_server(settings: Settings) -> Result<Server, AppError> {
    let pieces = Collaborators::new(&settings)?;
    let agent = build_agent_from(&pieces, &settings);

    let queue_impl = Arc::new(InMemoryPromptQueue::new(pieces.clock.clone()));
    let queue: SharedPromptQueue = queue_impl.clone();
    let leases: SharedLeaseManager = queue_impl;

    let hub = Arc::new(InMemoryResponseHub::new());
    let sink: SharedResponseSink = hub.clone();
    let responses: SharedResponseSource = hub;

    let pool = WorkerPool::new(
        queue.clone(),
        leases.clone(),
        sink,
        agent,
        WorkerConfig::default(),
    );
    let workers = pool.spawn();

    let state = AppState {
        queue,
        leases,
        responses,
        sessions: pieces.sessions,
    };

    Ok(Server {
        state,
        workers,
        http_addr: settings.http_addr,
    })
}

/// Run the server until `cancel` fires. Performs graceful shutdown of HTTP first, then
/// the worker pool — workers continue processing in-flight turns up to their per-turn
/// timeout before exiting.
pub async fn run_server(server: Server, cancel: CancellationToken) -> Result<(), AppError> {
    let Server {
        state,
        workers,
        http_addr,
    } = server;
    let app = router(state);
    let listener = tokio::net::TcpListener::bind(http_addr)
        .await
        .map_err(|source| AppError::Bind { http_addr, source })?;
    info!(http_addr = %http_addr, "http.listening");

    let shutdown = cancel.clone();
    let serve = axum::serve(listener, app).with_graceful_shutdown(async move {
        shutdown.cancelled().await;
    });

    if let Err(e) = serve.await {
        warn!(error = %e, "http.serve.error");
    }
    info!("http.shutdown.complete");
    workers.shutdown().await;
    info!("workers.shutdown.complete");
    Ok(())
}

fn build_provider(settings: &Settings) -> Result<SharedProvider, AppError> {
    let provider = AnthropicProvider::new(
        &settings.anthropic_api_key,
        settings.anthropic_base_url.clone(),
    )?;
    Ok(Arc::new(provider))
}

fn build_tools(settings: &Settings, http: Client) -> Result<ToolRegistry, AppError> {
    Ok(ToolRegistry::builder()
        .with(Arc::new(WebFetchTool::new()?))
        .with(Arc::new(WebSearchTool::new(
            http,
            settings.brave_search_api_key.clone(),
        )))
        .build())
}

fn build_http_client() -> Result<Client, reqwest::Error> {
    Client::builder()
        .timeout(HTTP_DEFAULT_TIMEOUT)
        .connect_timeout(HTTP_CONNECT_TIMEOUT)
        .user_agent(HTTP_USER_AGENT)
        .build()
}
