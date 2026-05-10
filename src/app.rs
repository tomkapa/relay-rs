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

use crate::agent_core::{Agent, AgentBuilder};
use crate::agents::{
    AGENT_PROMPT_CACHE_CAP, AGENT_PROMPT_CACHE_TTL, AgentFactory, AgentName, AgentPromptCache,
    AgentSystemPrompt, CachedAgents, DefaultAgentSeed, PgAgentStore, SharedAgentStore,
    SharedAgents,
};
use crate::clock::{SharedClock, SystemClock};
use crate::config::{ProviderSettings, Settings};
use crate::error::AppError;
use crate::hook::HookChain;
use crate::http::{AppState, router};
use crate::mcp::{McpRefresher, McpRegistry, PgMcpServerStore, SharedMcpServerStore};
use crate::memory::{AgentMemory, SharedMemory};
use crate::provider::SharedProvider;
use crate::provider::anthropic::AnthropicProvider;
use crate::provider::openai::OpenAiProvider;
use crate::runtime::{
    PgDagBudget, PgPromptQueue, PgResponseHub, PgThreadStream, SharedDagBudget, SharedLeaseManager,
    SharedPromptQueue, SharedResponseSink, SharedResponseSource, SharedThreadStream, WorkerConfig,
    WorkerPool, WorkerPoolHandle,
};
use crate::session::{PgSessionStore, SharedSessionStore};
use crate::tools::system::{self, SystemToolDeps};
use crate::tools::{ToolBox, ToolRegistry};

const HTTP_USER_AGENT: &str = concat!("relay-rs/", env!("CARGO_PKG_VERSION"));
const HTTP_DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
const HTTP_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

const PG_MAX_CONNECTIONS: u32 = 32;
const PG_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(5);

/// Universal personality prefix wrapped in `<core>...</core>` by
/// [`AgentMemory`] before the role block. Ships with the binary.
const CORE_SYSTEM_PROMPT: &str = "You are a thoughtful, professional teammate. \
    You aim for correctness, clarity, and pragmatism in every response. \
    You verify facts with tools when the answer is not already obvious, \
    you state what you intend before calling a tool, and you reason in small \
    steps before committing to an answer. You take feedback seriously, learn \
    from corrections, and grow toward senior-level judgement: you escalate \
    ambiguity, you flag unstated assumptions, and you never silently paper \
    over an error. \
    \n\n\
    Communication protocol — read this carefully:\n\
    \n\
    1. To deliver any message — to a human or to another agent — you MUST \
    call the `send_message` tool. Plain assistant text is treated as a \
    private thought; it is NOT delivered to anyone. Every turn that intends \
    to communicate MUST call `send_message`. A turn of pure thinking with \
    no `send_message` is rare and should be a deliberate decision.\n\
    \n\
    2. `send_message` is ASYNCHRONOUS. The receiver runs later, in a \
    separate worker turn. After your `send_message` call returns successfully \
    (the tool result will say `\"delivery\": \"queued\"` or \
    `\"published\"`), you have done your job for that hop. Emit one short \
    closing text — your private thought, not delivered — and END THE TURN. \
    DO NOT keep calling tools to wait for a reply.\n\
    \n\
    3. The receiver's reply will arrive in YOUR NEXT TURN automatically. \
    The system wakes you up with the receiver's message already visible in \
    the conversation history; you do not need to fetch it. There is no \
    `wait_for_reply` mechanism — turns are how you wait.\n\
    \n\
    4. NEVER call `get_session` to poll for a reply on a session you \
    just sent to. Polling does not make the receiver run faster; it just \
    spends turn budget on stale snapshots. The reply you are looking for \
    is not yet written when you ask. If you find yourself thinking \"let me \
    check if they replied\" — STOP. Emit a closing thought and end the \
    turn. The next time you run, you will see what they said.\n\
    \n\
    5. NEVER re-send the same message in the same turn. If `send_message` \
    returned successfully once, the message was queued; sending it again \
    just enqueues a duplicate. If your delegation needs no reply, send \
    once and end. If it needs a reply, send once, end the turn, and wait \
    for the next turn.\
    \n\n\
    Chain of communication — treat this like a real workplace:\n\
    \n\
    6. REPLY TO WHOEVER MESSAGED YOU. When another agent sends you a \
    question, your answer goes back to THAT agent — not to whoever they \
    report to, not to the human who originally started the task, not to \
    a peer you think is more relevant. The agent who contacted you owns \
    the relay back up the chain. Bypassing them to address their lead \
    or the human directly is impolite and breaks the chain. The system \
    permits it (any agent can call `send_message(receiver=Human, ...)`); \
    the etiquette forbids it without a reason.\n\
    \n\
    7. ESCALATE UP YOUR OWN LINE WHEN YOU NEED HELP. If you cannot \
    answer the request on your own, you MAY `send_message` to your own \
    lead, your manager, or a peer agent whose expertise you need. When \
    their reply arrives in a later turn, fold it into your own answer \
    and send THAT back to whoever asked you. Example: tech-lead asks \
    dev → dev asks marketing → marketing asks marketing-lead → \
    marketing-lead replies to marketing → marketing replies to dev → \
    dev replies to tech-lead. Each hop answers the hop above it; nobody \
    skips a level.\n\
    \n\
    8. ONLY MESSAGE THE HUMAN DIRECTLY WHEN APPROPRIATE. Direct \
    `send_message(receiver=Human, ...)` is correct when (a) the human \
    addressed YOU first — you are the agent the human's original \
    request was routed to, OR (b) the agent who messaged you has \
    explicitly asked you to report to the human. In every other case, \
    reply to the agent who messaged you and let your answer travel back \
    up the chain. When in doubt, reply to whoever asked you and let \
    them decide what to forward.";

const DEFAULT_AGENT_NAME: &str = "assistant";

/// Seed body for the default agent. Owned by the DB after first insert —
/// editing this constant does **not** update existing deployments.
const DEFAULT_AGENT_ROLE_PROMPT: &str = "You are the user's general assistant, \
    acting like a capable executive secretary. Help with whatever the user \
    asks: drafting, summarising, planning, looking things up, and following \
    through on multi-step tasks. Prefer concrete next steps over open-ended \
    musings, and ask one focused clarifying question when the request is \
    genuinely ambiguous.";

/// All the pieces a deployment needs to serve HTTP + run workers in-process.
#[derive(Debug)]
pub struct Server {
    pub state: AppState,
    pub workers: WorkerPoolHandle,
    pub mcp_refresher: McpRefresher,
    pub http_addr: SocketAddr,
}

/// Pre-built collaborators shared by the agent and the runtime.
#[derive(Debug)]
struct Collaborators {
    provider: SharedProvider,
    // Held to keep the pool alive for the server's lifetime. Concrete impls
    // (PgSessionStore, PgPromptQueue, …) each carry their own clone.
    #[allow(dead_code)]
    pool: PgPool,
    sessions: SharedSessionStore,
    agents: SharedAgentStore,
    memory: SharedMemory,
    clock: SharedClock,
    builtin_tools: ToolRegistry,
    queue: SharedPromptQueue,
    leases: SharedLeaseManager,
    dag: SharedDagBudget,
    sink: SharedResponseSink,
    responses: SharedResponseSource,
    mcp_store: SharedMcpServerStore,
    mcp_registry: Arc<McpRegistry>,
}

impl Collaborators {
    async fn new(settings: &Settings) -> Result<Self, AppError> {
        let http = build_http_client()?;
        let clock = SystemClock::shared();
        let pool = connect_pool(settings).await?;
        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .map_err(|source| AppError::Migrate { source })?;

        // Downstream (HTTP, worker, memory) assumes `agents.default_id()` resolves.
        let agents_impl = Arc::new(PgAgentStore::new(pool.clone(), clock.clone()));
        let seed = default_agent_seed()?;
        agents_impl.seed_default(seed).await?;
        let agents: SharedAgentStore = agents_impl;

        let sessions: SharedSessionStore =
            Arc::new(PgSessionStore::new(pool.clone(), clock.clone()));

        let cache = Arc::new(AgentPromptCache::new(
            AGENT_PROMPT_CACHE_CAP,
            AGENT_PROMPT_CACHE_TTL,
            clock.clone(),
        ));
        let memory: SharedMemory =
            Arc::new(AgentMemory::new(agents.clone(), cache, CORE_SYSTEM_PROMPT));

        let mcp_store: SharedMcpServerStore =
            Arc::new(PgMcpServerStore::new(pool.clone(), clock.clone()));
        let mcp_registry = McpRegistry::new(mcp_store.clone(), clock.clone());

        // Queue, DAG budget, and response hub are built here so the
        // `send_message` system tool can hold them without a later
        // round-trip. The hub's publish/subscribe halves split between
        // `send_message` (human-receiver branch) and the SSE route.
        let queue_impl = Arc::new(PgPromptQueue::new(pool.clone(), clock.clone()));
        let queue: SharedPromptQueue = queue_impl.clone();
        let leases: SharedLeaseManager = queue_impl;
        let dag: SharedDagBudget = Arc::new(PgDagBudget::new(pool.clone()));

        let hub = Arc::new(PgResponseHub::new(pool.clone(), clock.clone()));
        let sink: SharedResponseSink = hub.clone();
        let responses: SharedResponseSource = hub;

        let builtin_tools = system::register(
            ToolRegistry::builder(),
            SystemToolDeps {
                http,
                brave_search_api_key: settings.brave_search_api_key.clone(),
                sessions: sessions.clone(),
                queue: queue.clone(),
                dag: dag.clone(),
                agents: agents.clone(),
                sink: sink.clone(),
            },
        )?
        .build();

        Ok(Self {
            provider: build_provider(settings)?,
            pool,
            sessions,
            agents,
            memory,
            clock,
            builtin_tools,
            queue,
            leases,
            dag,
            sink,
            responses,
            mcp_store,
            mcp_registry,
        })
    }

    fn toolbox(&self) -> ToolBox {
        ToolBox::new(self.builtin_tools.clone(), self.mcp_registry.clone())
    }
}

fn default_agent_seed() -> Result<DefaultAgentSeed, AppError> {
    Ok(DefaultAgentSeed {
        name: AgentName::try_from(DEFAULT_AGENT_NAME)?,
        system_prompt: AgentSystemPrompt::try_from(DEFAULT_AGENT_ROLE_PROMPT)?,
    })
}

/// Build a fully-wired [`Agent`] without the HTTP/worker stack.
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
    // Every agent shares the same collaborators today; the factory seam is
    // here so a future change can specialise per-agent (model, tool subset).
    let template = build_agent_from(&pieces, &settings);
    let factory: AgentFactory = Arc::new(move |_record| template.clone());
    let agents_registry: SharedAgents = Arc::new(CachedAgents::new(
        pieces.agents.clone(),
        factory,
        AGENT_PROMPT_CACHE_CAP,
        AGENT_PROMPT_CACHE_TTL,
        pieces.clock.clone(),
    ));

    // Best-effort initial refresh — failure logged via `last_error` and
    // the warn logs inside `refresh`; doesn't block startup.
    if let Err(e) = pieces.mcp_registry.refresh().await {
        warn!(error = %e, "mcp.refresh.startup_failed");
    }

    let (mcp_refresher, mcp_refresh) = McpRefresher::spawn(pieces.mcp_registry.clone());

    let pool = WorkerPool::new(
        pieces.queue.clone(),
        pieces.leases.clone(),
        pieces.sink.clone(),
        agents_registry,
        pieces.sessions.clone(),
        pieces.dag.clone(),
        WorkerConfig::default(),
    );
    let workers = pool.spawn();

    // Single-process fan-in subscriber for the chat-UI thread stream. Owns
    // its own LISTEN connection on the shared pool; tied to the same cancel
    // token so a top-level shutdown winds the listener task down with the
    // rest of the runtime.
    let thread_stream: SharedThreadStream =
        PgThreadStream::spawn(pieces.pool.clone(), CancellationToken::new())
            .await
            .map_err(|source| AppError::DbConnect { source })?;

    let state = AppState {
        queue: pieces.queue,
        leases: pieces.leases,
        responses: pieces.responses,
        sessions: pieces.sessions,
        agents: pieces.agents,
        dag: pieces.dag,
        mcp_store: pieces.mcp_store,
        mcp_refresh,
        thread_stream,
        pool: pieces.pool.clone(),
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
    // HTTP first, then refresher (no in-flight upstream MCP calls), then workers.
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
