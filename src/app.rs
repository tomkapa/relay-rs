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
    AGENT_PROMPT_CACHE_CAP, AGENT_PROMPT_CACHE_TTL, AgentDescription, AgentFactory, AgentName,
    AgentNamesCache, AgentPromptCache, AgentStoreError, AgentSystemPrompt, CachedAgents,
    DefaultAgentSeed, PgAgentStore, SharedAgentStore, SharedAgents,
};
use crate::auth::{GoogleOAuth, JwtSigner, OrgId, PgUserStore, SharedUserStore};
use crate::clock::{SharedClock, SystemClock};
use crate::config::{EmbeddingSettings, ProviderSettings, Settings};
use crate::crypto::OrgEncryptor;
use crate::error::AppError;
use crate::hook::HookChain;
use crate::http::{AppState, router};
use crate::mcp::{
    McpRefresher, McpRegistry, PgMcpCredentialStore, PgMcpServerStore, ScopedMcpSource,
    SharedMcpCredentialStore, SharedMcpServerStore,
};
use crate::memory::{
    AgentMemory, LibrarianScheduler, MemorySectionLoader, ModeCores, PgMemoryStore,
    ReflectionScheduler, SESSION_MEMORY_CACHE_CAP, SESSION_MEMORY_CACHE_TTL_SECS,
    SessionMemoryCache, SharedMemory, SharedMemoryStore,
};
use crate::provider::anthropic::AnthropicProvider;
use crate::provider::openai::{OpenAiEmbeddingProvider, OpenAiProvider};
use crate::provider::{SharedEmbeddingProvider, SharedProvider};
use crate::runtime::{
    PgDagBudget, PgPromptQueue, PgResponseHub, PgThreadStream, SharedDagBudget, SharedLeaseManager,
    SharedPromptQueue, SharedResponseSink, SharedResponseSource, SharedThreadStream, WorkerConfig,
    WorkerPool, WorkerPoolHandle,
};
use crate::scheduling::{
    DefaultTimezone, PgScheduledTaskStore, ScheduledTaskScheduler, SharedScheduledTaskStore,
    Timezone,
};
use crate::session::{PgSessionStore, SharedSessionStore};
use crate::tools::system::{
    CancelScheduledTaskTool, CreateAgentTool, GetSessionTool, ListScheduledTasksTool,
    MemoryForgetTool, MemoryToolDeps, MemoryUpdateTool, MemoryValidateTool, MemoryWriteTool,
    RecallTool, SCHEDULING_CORE_PROMPT_SUPPLEMENT, ScheduleTaskTool, SearchAgentsTool,
    SendMessageTool, WebFetchTool, WebSearchTool,
};
use crate::tools::{ToolBox, ToolRegistry};

const HTTP_USER_AGENT: &str = concat!("relay-rs/", env!("CARGO_PKG_VERSION"));
const HTTP_DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
const HTTP_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

const PG_MAX_CONNECTIONS: u32 = 32;
const PG_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(5);

/// Universal `<core>` block wrapped by [`AgentMemory`] before the role
/// block. Ships with the binary — edits take effect on next process start
/// for every agent. (Contrast with [`DEFAULT_AGENT_ROLE_PROMPT`], which
/// only seeds the DB row; later edits to that constant do not propagate.)
///
/// Internally tagged into `<identity>`, `<communication>`, and
/// `<chain_of_command>` so each concern can be iterated independently
/// without rereading the others. The outer `<core>...</core>` envelope is
/// added by `AgentMemory::system_prompt`.
const CORE_SYSTEM_PROMPT: &str = "<identity>\n\
    You are a thoughtful, professional teammate. You aim for correctness, \
    clarity, and pragmatism in every response. You verify facts with tools \
    when the answer is not already obvious; you state what you intend \
    before calling a tool; and you reason in small steps before committing \
    to an answer. You take feedback seriously, learn from corrections, and \
    grow toward senior-level judgement — you escalate ambiguity, flag \
    unstated assumptions, and never silently paper over an error.\n\
    \n\
    The role block below may further specialise your persona, voice, or \
    domain. Defer to the role for style and focus. The rules in \
    `<communication>` and `<chain_of_command>` are invariants and apply \
    regardless of role.\n\
    </identity>\n\
    \n\
    <communication>\n\
    To deliver any message — to a human or another agent — call the \
    `send_message` tool. Plain assistant text is a private thought and is \
    not delivered to anyone. A turn that intends to communicate must call \
    `send_message`.\n\
    \n\
    `send_message` is asynchronous: the receiver runs later, in a separate \
    worker turn. Turns are how you wait. Once `send_message` returns \
    successfully, your job for that hop is done — emit one short private \
    closing thought and end the turn. The receiver's reply arrives in your \
    next turn automatically, with their message already visible in the \
    conversation history; there is no `wait_for_reply` mechanism.\n\
    \n\
    Corollaries:\n\
    - Do not call `get_session` to poll for a reply on a session you just \
    sent to — the reply is not yet written when you ask, and polling just \
    spends turn budget on stale snapshots. If you find yourself thinking \
    \"let me check if they replied,\" end the turn instead.\n\
    - Do not re-send the same message in the same turn. A successful \
    `send_message` already queued the message; sending again enqueues a \
    duplicate.\n\
    - If a tool call errors, do not retry it with the same arguments in \
    the same turn — surface the error in your closing thought so the next \
    turn (or an operator) can react.\n\
    </communication>\n\
    \n\
    <chain_of_command>\n\
    Treat the agent network like a real workplace.\n\
    \n\
    Reply to whoever messaged you. When another agent asks you a \
    question, your answer goes back to that agent — not to whoever they \
    report to, not to the human who started the original task, not to a \
    peer you think is more relevant. The agent who contacted you owns the \
    relay back up the chain. The system permits direct messages to the \
    human (`send_message(receiver=Human, ...)`); the etiquette forbids it \
    without reason.\n\
    \n\
    Escalate up your own line when you need help. If you cannot answer on \
    your own, `send_message` your lead, manager, or a peer whose expertise \
    you need. When their reply arrives in a later turn, fold it into your \
    own answer and send that back to whoever asked you. Each hop answers \
    the hop above it; nobody skips a level.\n\
    \n\
    Message the human directly only when (a) the human addressed you \
    first — you are the agent the human's original request was routed to, \
    or (b) the agent who messaged you has explicitly asked you to report \
    to the human. When in doubt, reply to whoever asked you and let them \
    decide what to forward.\n\
    \n\
    Choosing a recipient — the delegation order. When an incoming \
    message asks for work outside your role, do not attempt it; \
    delegate. Pick a recipient in this strict order:\n\
    \n\
    1. A name your role prompt directly names (your procedural peers).\n\
    2. A name your `<memory>` records as the right collaborator for this \
    kind of task (Collaborator-kind entries).\n\
    3. A name in `<agents>` whose role obviously fits the task.\n\
    4. The top result of `search_agents(query)` when steps 1–3 don't \
    yield a clear answer.\n\
    \n\
    If `search_agents` returns nothing relevant, `send_message` the human \
    asking which collaborator should own this — or whether to drop the \
    request. Do not improvise the work yourself.\n\
    \n\
    After a delegation that started with `search_agents` succeeds, write \
    a `memory_write(kind=\"collaborator\", ...)` recording who you used \
    and why, so future-you skips the search.\n\
    </chain_of_command>";

/// `<core>` block injected for `RequestKind::Reflection` turns. Reflects
/// on the conversation since the last checkpoint and uses the memory
/// mutation tools as needed; the LLM controls the loop and ends with
/// plain assistant text when it has nothing more to do.
const REFLECTION_CORE_PROMPT: &str = "You are reflecting on a recent conversation between turns. \
    Decide what — if anything — to remember, update, or forget about \
    yourself, the people and agents you spoke with, and the procedures \
    you used. Each memory you write must be one or two sentences and \
    independently meaningful when read in isolation.\n\
    \n\
    Use the memory mutation tools as needed; you may also `recall` to \
    check whether a similar memory already exists before writing. If \
    `recall` surfaces a memory that says the same thing you were about \
    to write, prefer `memory_update` over `memory_write` so you don't \
    create a duplicate. If the user explicitly affirmed an existing \
    memory in this conversation, call `memory_validate` with the user's \
    exact words as `evidence`. When you have nothing more to do, end \
    the turn with a brief plain-text sign-off (no tool call) and the \
    loop will stop.\n\
    \n\
    Be conservative. Most conversations need zero or one new memories. \
    If nothing in the conversation crossed the threshold of \"this is \
    worth carrying forward across sessions\", emit no tool calls and \
    end the turn.";

/// `<core>` block injected for `RequestKind::Resolution` turns.
///
/// The two flagged memories arrive in the user prompt body as `M-1`
/// (memory A) and `M-2` (memory B), with their provenance. The agent's
/// standard `<memory>` block still renders the stable + contextual
/// layers at `M-3..` so related memories can inform the decision. The
/// model decides to update one, forget one, or close with no action,
/// and may use `web_search`, `web_fetch`, `recall`, or `send_message`
/// to gather evidence before committing.
const RESOLUTION_CORE_PROMPT: &str = "You are resolving a contradiction in your memory. The two \
    flagged memories arrive in the incoming user message as M-1 and M-2 with \
    their provenance (kind, state, pinned, created/validated timestamps, \
    source turn). Your `<memory>` block below lists other related memories \
    at M-3 and beyond that may help you judge the conflict.\n\
    \n\
    Use `memory_update` on M-1 or M-2 to revise the one that is wrong, or \
    `memory_forget` to drop it. If both are correct in different contexts \
    and neither needs mutating, end the turn with a short plain-text \
    explanation — the system records the no-action close automatically \
    using your final reply as the rationale. You may also use `recall`, \
    `web_search`, `web_fetch`, or `send_message` to gather evidence or ask \
    a human for clarification before deciding.\n\
    \n\
    Take your time. Investigate, then commit. End the turn with a brief \
    plain-text sign-off (no tool call) once the contradiction is resolved.";

const DEFAULT_AGENT_NAME: &str = "recruiter";

/// Seed body for the default agent. Owned by the DB after first insert —
/// editing this constant does **not** update existing deployments.
///
/// Relay is positioned as a staffing service that supplies AI-agent
/// employees to a customer company. The recruiter is the only agent the
/// customer talks to before they have hired anyone, and its only job is
/// hiring + onboarding — never doing the customer's downstream work.
const DEFAULT_AGENT_ROLE_PROMPT: &str = "You are the recruiter for a staffing service that \
    supplies AI agents to a customer company. Your only job is hiring and onboarding new \
    agent employees. You do not do the customer's work yourself — that is what their hires \
    are for. You do not route messages or forward tasks to existing employees; the customer \
    talks to each hire directly in their own session once you have made the introduction.\n\
    \n\
    Run every conversation as a hiring intake:\n\
    \n\
    1. Scope the role. Ask focused questions until you can name the work in one concrete \
    sentence (\"a translator who turns English release notes into Japanese\", not \
    \"someone for languages\"). One question per turn at most.\n\
    \n\
    2. Check the bench first. Skim the `<agents>` block. If a listed name already \
    plausibly fits, surface it to the customer and ask whether they want to use that \
    employee instead of hiring — duplicates dilute the team. If `<agents>` is ambiguous, \
    call `search_agents` with the role description; if a clear match comes back, \
    recommend them and ask whether to proceed with the existing hire. Only when the \
    customer confirms no existing employee fits do you move to hiring.\n\
    \n\
    3. Draft the hire with the customer. Decide together:\n\
       - `name` — role-shaped, lowercase, e.g. `translator`, `release_editor`.\n\
       - `description` — one sentence other agents read when deciding whether to \
         delegate here. Operator-facing, model-readable.\n\
       - `system_prompt` — the role's voice and scope, AND the onboarding section. \
         The onboarding section must spell out:\n\
           * Who this employee reports to (usually the human; sometimes a named teammate).\n\
           * The named peers they should `send_message` for help, and what each peer is \
             good at. Lift these from the customer's description of how their team works.\n\
           * The escalation order — which of those peers is the right first stop for \
             which kind of question.\n\
           * What kinds of things the employee should pay attention to and remember as \
             they work (the memory subsystem captures these naturally from the \
             employee's own turns; the prompt's job is to point at *what* to keep).\n\
       Read the full draft back to the customer and wait for an explicit \"go\" before \
       calling `create_agent`.\n\
    \n\
    4. Hire and hand off. Call `create_agent` with the agreed fields. When it returns, \
    tell the customer: the role is hired, here is the name, open a new session with that \
    name when they're ready to give it work. Do not `send_message` the new hire from \
    this session — the customer drives the first real conversation themselves.\n\
    \n\
    If the customer asks for something that is not hiring (a question, some work, a \
    status check), say so plainly: this room is for hiring; they should talk to the \
    relevant employee in that employee's own session. Do not improvise the work yourself.";

/// Operator-facing, model-readable one-line description for the default
/// agent. Seeded alongside the role prompt; later operator edits to this
/// constant do **not** propagate (owned by the DB after first insert).
const DEFAULT_AGENT_DESCRIPTION: &str = "Hiring agent for the staffing service. Talks with \
    the customer to scope a role, checks whether an existing employee already fits, and \
    creates a new agent with an onboarding-shaped system prompt when no one does. First \
    contact for any new conversation.";

/// All the pieces a deployment needs to serve HTTP + run workers in-process.
#[derive(Debug)]
pub struct Server {
    pub state: AppState,
    pub workers: WorkerPoolHandle,
    pub mcp_refresher: McpRefresher,
    pub reflection_scheduler: ReflectionScheduler,
    pub librarian_scheduler: LibrarianScheduler,
    pub scheduling_scheduler: ScheduledTaskScheduler,
    pub http_addr: SocketAddr,
}

/// Pre-built collaborators shared by the agent and the runtime.
#[derive(Debug)]
struct Collaborators {
    provider: SharedProvider,
    pool: PgPool,
    sessions: SharedSessionStore,
    agents: SharedAgentStore,
    memory: SharedMemory,
    memory_store: SharedMemoryStore,
    clock: SharedClock,
    builtin_tools: ToolRegistry,
    queue: SharedPromptQueue,
    leases: SharedLeaseManager,
    dag: SharedDagBudget,
    sink: SharedResponseSink,
    responses: SharedResponseSource,
    mcp_store: SharedMcpServerStore,
    mcp_credentials: SharedMcpCredentialStore,
    mcp_registry: McpRegistry,
    scheduled_tasks: SharedScheduledTaskStore,
}

impl Collaborators {
    // Composition root: a straight-line constructor that wires every
    // collaborator once. The line cap (CLAUDE.md §4) targets logic
    // functions; this one is configuration plus binding, not branching.
    #[allow(clippy::too_many_lines)]
    async fn new(settings: &Settings) -> Result<Self, AppError> {
        let http = build_http_client()?;
        let clock = SystemClock::shared();
        let pool = connect_pool(settings).await?;
        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .map_err(|source| AppError::Migrate { source })?;

        let embedding_provider: SharedEmbeddingProvider =
            build_embedding_provider(&settings.embedding);

        // Per-org default-agent seeding happens lazily on first sign-up
        // (see `seed_default_agent_for_org` and `auth::callback`); the
        // composition root no longer mints a global default because there
        // is no global org to own it.
        let agents_impl = Arc::new(PgAgentStore::new(
            pool.clone(),
            clock.clone(),
            embedding_provider.clone(),
        ));
        let agents: SharedAgentStore = agents_impl;

        let sessions: SharedSessionStore =
            Arc::new(PgSessionStore::new(pool.clone(), clock.clone()));

        let cache = AgentPromptCache::new(
            AGENT_PROMPT_CACHE_CAP,
            AGENT_PROMPT_CACHE_TTL,
            clock.clone(),
        );
        let names_cache = AgentNamesCache::new(
            AGENT_PROMPT_CACHE_CAP,
            AGENT_PROMPT_CACHE_TTL,
            clock.clone(),
        );
        let memory_store: SharedMemoryStore = Arc::new(PgMemoryStore::new(
            pool.clone(),
            clock.clone(),
            embedding_provider.clone(),
        ));
        let session_memory_cache = SessionMemoryCache::new(
            SESSION_MEMORY_CACHE_CAP,
            Duration::from_secs(SESSION_MEMORY_CACHE_TTL_SECS),
            clock.clone(),
        );
        // One loader, two consumers — `AgentMemory` (system-prompt
        // assembly) and `MemoryToolDeps` (handle resolution inside the
        // mutation tools). Sharing the loader is what keeps the
        // contextual layer consistent across the two paths that write
        // into the same `(session, agent)` cache key.
        let memory_loader = MemorySectionLoader::new(
            memory_store.clone(),
            sessions.clone(),
            embedding_provider.clone(),
            session_memory_cache.clone(),
        );
        let normal_core = format!("{CORE_SYSTEM_PROMPT}{SCHEDULING_CORE_PROMPT_SUPPLEMENT}");
        let cores = ModeCores {
            normal: Arc::<str>::from(normal_core),
            reflection: Arc::from(REFLECTION_CORE_PROMPT),
            resolution: Arc::from(RESOLUTION_CORE_PROMPT),
        };
        let memory: SharedMemory = Arc::new(AgentMemory::new(
            agents.clone(),
            cache,
            names_cache,
            memory_loader.clone(),
            cores,
            clock.clone(),
        ));

        let mcp_store: SharedMcpServerStore =
            Arc::new(PgMcpServerStore::new(pool.clone(), clock.clone()));
        let encryptor = Arc::new(
            OrgEncryptor::from_settings(&settings.auth.master_kek)
                .map_err(|e| AppError::Misconfigured(format!("RELAY_MASTER_KEK: {e}")))?,
        );
        let mcp_credentials: SharedMcpCredentialStore = Arc::new(PgMcpCredentialStore::new(
            pool.clone(),
            clock.clone(),
            encryptor,
        ));
        let mcp_registry = McpRegistry::with_credentials(
            mcp_store.clone(),
            Some(mcp_credentials.clone()),
            clock.clone(),
        );

        let scheduled_tasks: SharedScheduledTaskStore =
            Arc::new(PgScheduledTaskStore::new(pool.clone(), clock.clone()));
        let default_tz =
            DefaultTimezone::from_timezone(Timezone::from_tz(settings.default_timezone));

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

        // Built-in tool registration. Each tool's constructor is
        // straightforward; keeping the registration in the composition
        // root avoids a register-helper that just ferries a deps
        // struct in from this same call site. Adding a tool is one new
        // file in `tools/system/` + one `.with(...)` line here.
        // The memory tool family shares one set of deps. `recall` is the
        // only memory tool that talks to the embedding provider directly;
        // the mutation tools embed through `MemoryStore`. `memory_update`
        // and `memory_forget` close the active contradiction inline when
        // the worker is dispatching a resolution turn (via
        // `ToolCallContext::resolution_target`); the no-action close runs
        // post-turn in the worker.
        let memory_tools = MemoryToolDeps::new(memory_loader);
        let builtin_tools = build_builtin_tools(BuiltinToolDeps {
            http,
            settings,
            sessions: sessions.clone(),
            queue: queue.clone(),
            dag: dag.clone(),
            agents: agents.clone(),
            sink: sink.clone(),
            memory_tools,
            embedding_provider,
            scheduled_tasks: scheduled_tasks.clone(),
            default_tz,
            clock: clock.clone(),
            pool: pool.clone(),
        })?;

        // `memory_store` and `session_memory_cache` are not held on
        // `Collaborators`: they're cheap-clone handles already
        // distributed to every consumer (AgentMemory and the memory
        // tools) by the clones above. The reflection scheduler builds
        // its own handles from `pieces.pool` / `pieces.queue` later.
        Ok(Self {
            provider: build_provider(settings)?,
            pool,
            sessions,
            agents,
            memory,
            memory_store,
            clock,
            builtin_tools,
            queue,
            leases,
            dag,
            sink,
            responses,
            mcp_store,
            mcp_credentials,
            mcp_registry,
            scheduled_tasks,
        })
    }
}

/// Just enough of the collaborator graph to assemble one `Agent` for one
/// `AgentRecord`. Cheap to clone (every field is either `Arc`-wrapped or
/// already `Clone` over cheap state) so the factory closure can hold it.
#[derive(Clone)]
struct AgentFactoryPieces {
    provider: SharedProvider,
    sessions: SharedSessionStore,
    memory: SharedMemory,
    clock: SharedClock,
    builtin_tools: ToolRegistry,
    mcp_registry: McpRegistry,
    model: crate::types::ModelId,
}

impl AgentFactoryPieces {
    fn build(&self, record: &crate::agents::AgentRecord) -> Agent {
        let dynamic = Arc::new(ScopedMcpSource::new(
            self.mcp_registry.clone(),
            &record.allowed_mcp_servers,
        ));
        let toolbox = ToolBox::new(self.builtin_tools.clone(), dynamic);
        AgentBuilder::new(
            self.provider.clone(),
            self.sessions.clone(),
            self.memory.clone(),
            self.model.clone(),
        )
        .expect("invariant: limits constants are static and parse")
        .with_tools(toolbox)
        .with_hooks(HookChain::new())
        .with_clock(self.clock.clone())
        .build()
    }
}

/// Aggregated handles passed to [`build_builtin_tools`]. Keeps the
/// `Collaborators::new` body under the §4 line cap by lifting tool
/// registration into its own function.
struct BuiltinToolDeps<'a> {
    http: Client,
    settings: &'a Settings,
    sessions: SharedSessionStore,
    queue: SharedPromptQueue,
    dag: SharedDagBudget,
    agents: SharedAgentStore,
    sink: SharedResponseSink,
    memory_tools: MemoryToolDeps,
    embedding_provider: SharedEmbeddingProvider,
    scheduled_tasks: SharedScheduledTaskStore,
    default_tz: DefaultTimezone,
    clock: SharedClock,
    /// Pool handle threaded through to the scheduling tools so they can
    /// open `begin_as_user` tx for tenant-side visibility gating.
    pool: PgPool,
}

/// Register every system tool into a [`ToolRegistry`]. Lives at the
/// composition root so adding a tool is one new file in `tools/system/`
/// + one `.with(...)` line here.
fn build_builtin_tools(deps: BuiltinToolDeps<'_>) -> Result<ToolRegistry, AppError> {
    Ok(ToolRegistry::builder()
        .with(Arc::new(WebFetchTool::new()?))
        .with(Arc::new(WebSearchTool::new(
            deps.http,
            deps.settings.brave_search_api_key.clone(),
        )))
        .with(Arc::new(SendMessageTool::new(
            deps.sessions.clone(),
            deps.queue.clone(),
            deps.dag.clone(),
            deps.agents.clone(),
            deps.sink.clone(),
        )))
        .with(Arc::new(GetSessionTool::new(deps.sessions.clone())))
        .with(Arc::new(MemoryWriteTool::new(deps.memory_tools.clone())))
        .with(Arc::new(MemoryUpdateTool::new(deps.memory_tools.clone())))
        .with(Arc::new(MemoryForgetTool::new(deps.memory_tools.clone())))
        .with(Arc::new(MemoryValidateTool::new(deps.memory_tools.clone())))
        .with(Arc::new(RecallTool::new(
            deps.memory_tools,
            deps.embedding_provider.clone(),
        )))
        .with(Arc::new(SearchAgentsTool::new(
            deps.agents.clone(),
            deps.embedding_provider,
        )))
        .with(Arc::new(CreateAgentTool::new(deps.agents.clone())))
        .with(Arc::new(ScheduleTaskTool::new(
            deps.scheduled_tasks.clone(),
            deps.agents.clone(),
            deps.sessions.clone(),
            deps.default_tz,
            deps.clock,
        )))
        .with(Arc::new(ListScheduledTasksTool::new(
            deps.scheduled_tasks.clone(),
            deps.sessions.clone(),
            deps.pool.clone(),
        )))
        .with(Arc::new(CancelScheduledTaskTool::new(
            deps.scheduled_tasks,
            deps.sessions.clone(),
            deps.pool.clone(),
        )))
        .build())
}

/// Seed material for the per-org default agent.
///
/// Exposed so the OAuth callback (`auth::callback`) can mint the default
/// agent inside the just-created personal org without re-importing the
/// recruiter prompt constants. Fails only if one of the seed constants
/// suddenly violates a newtype invariant — a compile-time-ish guarantee
/// in practice.
pub fn default_agent_seed() -> Result<DefaultAgentSeed, AppError> {
    Ok(DefaultAgentSeed {
        name: AgentName::try_from(DEFAULT_AGENT_NAME)?,
        system_prompt: AgentSystemPrompt::try_from(DEFAULT_AGENT_ROLE_PROMPT)?,
        description: AgentDescription::try_from(DEFAULT_AGENT_DESCRIPTION)?,
    })
}

/// Seed the default agent for `org_id`.
///
/// Idempotent — a second call for the same org returns the existing
/// default's id. Called from the OAuth callback on first sign-up so the
/// cookie minted immediately resolves to a usable workspace.
pub async fn seed_default_agent_for_org(
    agents: &SharedAgentStore,
    org_id: OrgId,
    seed: DefaultAgentSeed,
) -> Result<crate::agents::AgentId, AgentStoreError> {
    agents.seed_default(org_id, seed).await
}

/// Build a fully-wired [`Agent`] without the HTTP/worker stack.
///
/// Skips per-agent MCP scoping (no `AgentRecord` to scope against here) —
/// callers must not use this for production turn dispatch, which goes
/// through `build_server`'s per-agent factory below.
pub async fn build_agent(settings: Settings) -> Result<Agent, AppError> {
    let pieces = Collaborators::new(&settings).await?;
    Ok(build_agent_from(&pieces, &settings))
}

fn build_agent_from(pieces: &Collaborators, settings: &Settings) -> Agent {
    let toolbox = ToolBox::new(
        pieces.builtin_tools.clone(),
        pieces.mcp_registry.as_dynamic_source(),
    );
    AgentBuilder::new(
        pieces.provider.clone(),
        pieces.sessions.clone(),
        pieces.memory.clone(),
        settings.model.clone(),
    )
    .expect("invariant: limits constants are static and parse")
    .with_tools(toolbox)
    .with_hooks(HookChain::new())
    .with_clock(pieces.clock.clone())
    .build()
}

/// Build the full HTTP + worker pool composition. The returned [`Server`] is ready to
/// hand to `axum::serve` and a graceful-shutdown loop.
#[allow(clippy::too_many_lines)] // composition root: configuration + binding, not branching
pub async fn build_server(
    settings: Settings,
    cancel: CancellationToken,
) -> Result<Server, AppError> {
    let pieces = Collaborators::new(&settings).await?;
    // Per-agent MCP scope: the factory reads the row's `allowed_mcp_servers`
    // and builds a `ScopedMcpSource` so the agent's `ToolBox` only sees the
    // permitted servers' tools. Everything else (provider, sessions, memory,
    // builtins, hooks) is cheap-clone shared across agents.
    let factory_pieces = AgentFactoryPieces {
        provider: pieces.provider.clone(),
        sessions: pieces.sessions.clone(),
        memory: pieces.memory.clone(),
        clock: pieces.clock.clone(),
        builtin_tools: pieces.builtin_tools.clone(),
        mcp_registry: pieces.mcp_registry.clone(),
        model: settings.model.clone(),
    };
    let factory: AgentFactory = Arc::new(move |record| factory_pieces.build(record));
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
        pieces.pool.clone(),
        pieces.memory_store.clone(),
        pieces.clock.clone(),
        WorkerConfig::default(),
    );
    let workers = pool.spawn();

    // Background scheduler that periodically enqueues reflection turns
    // (doc/memory.md §1.6). The reflection itself runs through the worker
    // pool above.
    let reflection_scheduler = ReflectionScheduler::spawn(
        pieces.pool.clone(),
        pieces.queue.clone(),
        pieces.clock.clone(),
        cancel.clone(),
    );

    // Librarian — mechanical sweep per agent, plus resolution-turn
    // enqueue for unresolved contradictions (doc/memory.md §1.8).
    let librarian_scheduler = LibrarianScheduler::spawn(
        pieces.pool.clone(),
        pieces.memory_store.clone(),
        pieces.queue.clone(),
        pieces.clock.clone(),
        cancel.clone(),
    );

    // Scheduling — agent-driven scheduled tasks. Polls the
    // `scheduled_tasks` table on a fixed cadence and enqueues a
    // `prompt_requests` row for each due fire.
    let scheduling_scheduler = ScheduledTaskScheduler::spawn(
        pieces.scheduled_tasks.clone(),
        pieces.queue.clone(),
        pieces.clock.clone(),
        cancel.clone(),
    );

    // Single-process fan-in subscriber for the chat-UI thread stream. Owns
    // its own LISTEN connection on the shared pool; tied to the same cancel
    // token so a top-level shutdown winds the listener task down with the
    // rest of the runtime.
    let thread_stream: SharedThreadStream =
        PgThreadStream::spawn(pieces.pool.clone(), CancellationToken::new())
            .await
            .map_err(|source| AppError::DbConnect { source })?;

    let jwt =
        JwtSigner::new(&settings.auth.jwt_secret, pieces.clock.clone()).map_err(AppError::Auth)?;
    let oauth = GoogleOAuth::new(
        &settings.auth.google_client_id,
        &settings.auth.google_client_secret,
        &settings.auth.google_redirect_url,
    )
    .map_err(AppError::Auth)?;
    let users: SharedUserStore = Arc::new(PgUserStore::new(pieces.pool.clone()));

    let memberships = Arc::new(crate::http::MembershipCache::new(pieces.clock.clone()));
    let mcp_test_rate = crate::mcp::TestConnectRateLimiter::new(pieces.clock.clone());
    let state = AppState {
        queue: pieces.queue,
        leases: pieces.leases,
        responses: pieces.responses,
        sessions: pieces.sessions,
        agents: pieces.agents,
        dag: pieces.dag,
        memory_store: pieces.memory_store.clone(),
        mcp_store: pieces.mcp_store,
        mcp_credentials: pieces.mcp_credentials,
        mcp_refresh,
        mcp_test_rate,
        thread_stream,
        pool: pieces.pool.clone(),
        jwt,
        oauth,
        users,
        clock: pieces.clock.clone(),
        cookie_secure: settings.auth.cookie_secure,
        memberships,
    };

    Ok(Server {
        state,
        workers,
        mcp_refresher,
        reflection_scheduler,
        librarian_scheduler,
        scheduling_scheduler,
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
        reflection_scheduler,
        librarian_scheduler,
        scheduling_scheduler,
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
    // HTTP first, then schedulers (no new enqueues), then workers.
    reflection_scheduler.shutdown().await;
    info!("reflection_scheduler.shutdown.complete");
    librarian_scheduler.shutdown().await;
    info!("librarian_scheduler.shutdown.complete");
    scheduling_scheduler.shutdown().await;
    info!("scheduling_scheduler.shutdown.complete");
    mcp_refresher.shutdown().await;
    info!("mcp.refresher.shutdown.complete");
    workers.shutdown().await;
    info!("workers.shutdown.complete");
    Ok(())
}

fn build_embedding_provider(s: &EmbeddingSettings) -> SharedEmbeddingProvider {
    Arc::new(OpenAiEmbeddingProvider::new(
        &s.api_key,
        s.base_url.clone(),
        s.model.clone(),
        s.dimensions,
    ))
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
