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
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::agent::{Agent, AgentBuilder};
use crate::clock::{SharedClock, SystemClock};
use crate::config::{ProviderSettings, Settings};
use crate::error::AppError;
use crate::hook::HookChain;
use crate::http::{AppState, router};
use crate::mcp::{McpRefresher, McpRegistry, PgMcpServerStore, SharedMcpServerStore};
use crate::memory::{SharedMemory, StaticMemory};
use crate::provider::SharedProvider;
use crate::provider::anthropic::AnthropicProvider;
use crate::provider::openai::OpenAiProvider;
use crate::runtime::{
    PgPromptQueue, PgResponseHub, SharedLeaseManager, SharedPromptQueue, SharedResponseSink,
    SharedResponseSource, WorkerConfig, WorkerPool, WorkerPoolHandle,
};
use crate::session::{PgSessionStore, SharedSessionStore};
use crate::tools::{ToolBox, ToolRegistry, WebFetchTool, WebSearchTool};

const HTTP_USER_AGENT: &str = concat!("relay-rs/", env!("CARGO_PKG_VERSION"));
const HTTP_DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
const HTTP_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Pool sizing — each `relay-rs` process holds a single Postgres pool shared across
/// the worker pool, the session store, and the response hub. CLAUDE.md §9: sized at
/// startup; never grown on demand from a hot path.
const PG_MAX_CONNECTIONS: u32 = 32;
const PG_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(5);

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
    /// Owned handle for the MCP refresh coordinator (CLAUDE.md §7 — no floating tasks).
    /// Held here so graceful shutdown can stop it before the workers wind down.
    pub mcp_refresher: McpRefresher,
    pub http_addr: SocketAddr,
}

/// Pre-built collaborators shared by the agent and the runtime. Constructed once at
/// startup and consumed by [`build_agent_from`] / [`build_server`].
#[derive(Debug)]
struct Collaborators {
    provider: SharedProvider,
    pool: PgPool,
    sessions: SharedSessionStore,
    memory: SharedMemory,
    clock: SharedClock,
    builtin_tools: ToolRegistry,
    mcp_store: SharedMcpServerStore,
    mcp_registry: Arc<McpRegistry>,
}

impl Collaborators {
    async fn new(settings: &Settings) -> Result<Self, AppError> {
        let http = build_http_client()?;
        let clock = SystemClock::shared();
        let pool = connect_pool(settings).await?;
        // Run migrations on startup so a fresh deploy picks up the schema without an
        // operator step. CLAUDE.md §14: every change ships with a forward migration;
        // this loop applies them in order.
        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .map_err(|source| AppError::Migrate { source })?;
        let mcp_store: SharedMcpServerStore =
            Arc::new(PgMcpServerStore::new(pool.clone(), clock.clone()));
        let mcp_registry = McpRegistry::new(mcp_store.clone(), clock.clone());
        Ok(Self {
            provider: build_provider(settings)?,
            pool: pool.clone(),
            sessions: Arc::new(PgSessionStore::new(pool, clock.clone())),
            memory: Arc::new(StaticMemory::new(DEFAULT_SYSTEM_PROMPT)),
            clock,
            builtin_tools: build_tools(settings, http)?,
            mcp_store,
            mcp_registry,
        })
    }

    fn toolbox(&self) -> ToolBox {
        ToolBox::new(self.builtin_tools.clone(), self.mcp_registry.clone())
    }
}

/// Build a fully-wired [`Agent`] from configuration. Kept for tests and any consumer
/// that wants the agent without the HTTP/worker stack.
pub async fn build_agent(settings: Settings) -> Result<Agent, AppError> {
    let pieces = Collaborators::new(&settings).await?;
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
    .with_tools(pieces.toolbox())
    .with_hooks(HookChain::new())
    .with_clock(pieces.clock.clone())
    .build()
}

/// Build the full HTTP + worker pool composition. The returned [`Server`] is ready to
/// hand to `axum::serve` and a graceful-shutdown loop.
pub async fn build_server(settings: Settings) -> Result<Server, AppError> {
    let pieces = Collaborators::new(&settings).await?;
    let agent = build_agent_from(&pieces, &settings);

    // Best-effort initial refresh: connect to every registered MCP server so the very
    // first turn already sees their tools. A failure here doesn't block startup —
    // operator visibility comes from `last_error` columns and the warn logs inside
    // `refresh`. Empty / new deploys take the fast path with no upstream calls.
    if let Err(e) = pieces.mcp_registry.refresh().await {
        warn!(error = %e, "mcp.refresh.startup_failed");
    }

    // Spin the long-running refresh coordinator. Its JoinHandle is owned by `Server`
    // (§7) and the trigger is cloned into `AppState` so CRUD handlers fire it without
    // ever spawning a task themselves.
    let (mcp_refresher, mcp_refresh) = McpRefresher::spawn(pieces.mcp_registry.clone());

    let queue_impl = Arc::new(PgPromptQueue::new(
        pieces.pool.clone(),
        pieces.clock.clone(),
    ));
    let queue: SharedPromptQueue = queue_impl.clone();
    let leases: SharedLeaseManager = queue_impl;

    let hub = Arc::new(PgResponseHub::new(
        pieces.pool.clone(),
        pieces.clock.clone(),
    ));
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
        mcp_store: pieces.mcp_store,
        mcp_refresh,
    };

    Ok(Server {
        state,
        workers,
        mcp_refresher,
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
        mcp_refresher,
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
    // HTTP first (no more incoming refresh signals), then refresher (no in-flight
    // upstream MCP calls), then workers.
    mcp_refresher.shutdown().await;
    info!("mcp.refresher.shutdown.complete");
    workers.shutdown().await;
    info!("workers.shutdown.complete");
    Ok(())
}

fn build_provider(settings: &Settings) -> Result<SharedProvider, AppError> {
    let provider: SharedProvider = match &settings.provider {
        ProviderSettings::Openai { api_key, base_url } => {
            Arc::new(OpenAiProvider::new(api_key, base_url.clone()))
        }
        ProviderSettings::Anthropic { api_key, base_url } => {
            Arc::new(AnthropicProvider::new(api_key, base_url.clone())?)
        }
    };
    Ok(provider)
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

async fn connect_pool(settings: &Settings) -> Result<PgPool, AppError> {
    PgPoolOptions::new()
        .max_connections(PG_MAX_CONNECTIONS)
        .acquire_timeout(PG_ACQUIRE_TIMEOUT)
        .connect(settings.database_url.expose())
        .await
        .map_err(|source| AppError::DbConnect { source })
}
