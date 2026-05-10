//! Memory-mutation tools (doc/memory.md §1.5, §2.3 — Phase 3).
//!
//! Three tools share a counter so a single turn cannot blow past
//! [`MAX_MEMORY_MUTATIONS_PER_TURN`] across them combined:
//!
//! - `memory_write(kind, content)` — mints a new memory at `Tentative`.
//! - `memory_update(handle, content)` — replaces content; resets state
//!   to `Tentative`. Pinned rows reject agent edits.
//! - `memory_forget(handle)` — drops the materialized row; the journal
//!   keeps the event so reverts (Phase 8) work. Pinned rows reject agent
//!   forgets.
//!
//! `recall` is deferred to Phase 9 (needs the embedding provider).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tracing::{debug, warn};

use crate::agents::AgentId;
use crate::memory::{
    MAX_MEMORY_MUTATIONS_PER_TURN, MemoryContent, MemoryHandle, MemoryId, MemoryKind,
    MemoryMutation, MemoryState, MemoryStoreError, MutationSource, SessionMemoryCache,
    SharedMemoryStore, compose_memory_section,
};
use crate::runtime::PromptRequestId;
use crate::session::SessionId;
use crate::types::{ParseError, ToolName};

use super::super::traits::{Tool, ToolCallContext, ToolError};

const WRITE_TOOL_NAME: &str = "memory_write";
const UPDATE_TOOL_NAME: &str = "memory_update";
const FORGET_TOOL_NAME: &str = "memory_forget";

const WRITE_DESCRIPTION: &str = "Persist a new memory the agent should carry across sessions. \
     Memory is the agent's distilled understanding of itself, its peers, and learned \
     procedures — not raw conversation transcript.\n\
     \n\
     USE WHEN the user (or a peer agent) explicitly asks you to remember something — \
     \"remember I prefer tabs over spaces\", \"keep in mind we deploy on Mondays\". \
     DO NOT use on weak or implicit signals; reflection turns capture those.\n\
     \n\
     Arguments: `kind` is one of \"self\" (identity, style, preferences), \"other\" \
     (beliefs about specific peers), \"procedure\" (learned how-tos), \"open\" \
     (known unknowns); `content` is one or two sentences (max 4096 bytes). New \
     memories land at `tentative` state and need independent confirmation to promote.";

const UPDATE_DESCRIPTION: &str = "Revise an existing memory by handle. Use when a stored memory is wrong or \
     stale — \"your memory M-12 is wrong, the deadline is Tuesday\".\n\
     \n\
     The handle (e.g. `M-12`) comes from the `## Memory` section in your system \
     prompt. Updating resets the memory to `tentative` (a content change is, by \
     definition, unverified again). Pinned memories reject agent updates.\n\
     \n\
     Arguments: `handle` (the `M-NN` form), `content` (one or two sentences, max \
     4096 bytes).";

const FORGET_DESCRIPTION: &str = "Remove an existing memory by handle. Use when a stored memory is obsolete or \
     the user asks you to forget it — \"forget M-44, that's no longer how we do \
     it\". The journal retains the event so an operator can revert; the memory \
     stops appearing in your system prompt immediately. Pinned memories reject \
     agent forgets.\n\
     \n\
     Arguments: `handle` (the `M-NN` form).";

#[derive(Debug, Deserialize)]
struct WriteInput {
    kind: MemoryKind,
    content: String,
}

#[derive(Debug, Deserialize)]
struct UpdateInput {
    handle: String,
    content: String,
}

#[derive(Debug, Deserialize)]
struct ForgetInput {
    handle: String,
}

#[derive(Debug, Serialize)]
struct WriteOutput {
    memory_id: MemoryId,
    kind: &'static str,
    state: &'static str,
    note: &'static str,
}

#[derive(Debug, Serialize)]
struct UpdateOutput {
    memory_id: MemoryId,
    handle: String,
    state: &'static str,
}

#[derive(Debug, Serialize)]
struct ForgetOutput {
    memory_id: MemoryId,
    handle: String,
    status: &'static str,
}

/// Per-turn mutation counter shared by the three tools.
///
/// Bounded HashMap keyed by request id — when a turn exceeds the cap,
/// further mutation calls return `InvalidInput`. Entries are evicted
/// in bulk once the map is full so memory cannot grow without bound
/// across long-running processes.
#[derive(Debug)]
pub(super) struct MutationCounter {
    inner: Mutex<HashMap<PromptRequestId, usize>>,
    cap_per_turn: usize,
    bookkeeping_max_entries: usize,
}

impl MutationCounter {
    fn new() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
            cap_per_turn: MAX_MEMORY_MUTATIONS_PER_TURN,
            // Sized so a busy steady state of in-flight turns each
            // counting toward the cap fits without churn; bulk-evicted
            // when the map fills up.
            bookkeeping_max_entries: 1024,
        }
    }

    fn try_increment(&self, request_id: PromptRequestId) -> Result<usize, CapExceeded> {
        let mut map = self
            .inner
            .lock()
            .expect("invariant: MutationCounter mutex never poisoned");
        if map.len() >= self.bookkeeping_max_entries && !map.contains_key(&request_id) {
            // Bulk-clear when the map fills up. Counts older than this
            // bookkeeping bound were already enforced live; clearing
            // does not retroactively let a turn exceed the cap.
            map.clear();
        }
        let entry = map.entry(request_id).or_insert(0);
        if *entry >= self.cap_per_turn {
            return Err(CapExceeded {
                cap: self.cap_per_turn,
            });
        }
        *entry += 1;
        Ok(*entry)
    }
}

#[derive(Debug)]
struct CapExceeded {
    cap: usize,
}

/// Shared infrastructure the three tools hold a handle on — the store,
/// the per-session handle map, and the per-turn counter.
#[derive(Debug, Clone)]
pub struct MemoryToolDeps {
    pub store: SharedMemoryStore,
    pub session_cache: Arc<SessionMemoryCache>,
    pub(super) counter: Arc<MutationCounter>,
}

impl MemoryToolDeps {
    /// Construct from the storage seam + the session cache. The mutation
    /// counter is private to this module.
    #[must_use]
    pub fn new(store: SharedMemoryStore, session_cache: Arc<SessionMemoryCache>) -> Self {
        Self {
            store,
            session_cache,
            counter: Arc::new(MutationCounter::new()),
        }
    }
}

/// `memory_write` tool — mints a new agent-owned memory.
pub struct MemoryWriteTool {
    name: ToolName,
    description: &'static str,
    input_schema: Arc<Value>,
    deps: MemoryToolDeps,
}

impl std::fmt::Debug for MemoryWriteTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryWriteTool").finish_non_exhaustive()
    }
}

impl MemoryWriteTool {
    #[must_use]
    pub fn new(deps: MemoryToolDeps) -> Self {
        let name = ToolName::try_from(WRITE_TOOL_NAME).expect("valid tool name");
        let input_schema = Arc::new(json!({
            "type": "object",
            "required": ["kind", "content"],
            "properties": {
                "kind": { "type": "string", "enum": ["self", "other", "procedure", "open"] },
                "content": { "type": "string", "minLength": 1, "maxLength": 4096 }
            },
            "additionalProperties": false,
        }));
        Self {
            name,
            description: WRITE_DESCRIPTION,
            input_schema,
            deps,
        }
    }
}

#[async_trait]
impl Tool for MemoryWriteTool {
    fn name(&self) -> &ToolName {
        &self.name
    }
    fn description(&self) -> &str {
        self.description
    }
    fn input_schema(&self) -> Arc<Value> {
        self.input_schema.clone()
    }
    async fn execute(&self, _input: Value) -> Result<String, ToolError> {
        Err(ToolError::InvalidInput(
            "memory_write requires per-call context; invoke via execute_with_ctx".into(),
        ))
    }
    async fn execute_with_ctx(
        &self,
        input: Value,
        ctx: &ToolCallContext,
    ) -> Result<String, ToolError> {
        let parsed: WriteInput = serde_json::from_value(input)?;
        let agent = expect_agent(ctx)?;
        check_cap(&self.deps.counter, ctx.request_id)?;

        let content = MemoryContent::try_from(parsed.content).map_err(parse_to_tool_err)?;
        let outcome = self
            .deps
            .store
            .apply(MemoryMutation::Write {
                agent,
                kind: parsed.kind,
                content,
                state: MemoryState::Tentative,
                pinned: false,
                source: MutationSource::Turn(ctx.request_id),
            })
            .await
            .map_err(store_to_tool_err)?;

        debug!(
            relay.session.id = %ctx.session_id,
            relay.agent.id = %agent,
            relay.memory.id = %outcome.memory_id,
            relay.memory.kind = parsed.kind.as_str(),
            "memory_write.ok",
        );

        let out = WriteOutput {
            memory_id: outcome.memory_id,
            kind: parsed.kind.as_str(),
            state: MemoryState::Tentative.as_str(),
            note: "Memory written. The new entry will appear under a stable handle on your next session.",
        };
        Ok(serde_json::to_string(&out)?)
    }
}

/// `memory_update` tool — revises an existing memory's content.
pub struct MemoryUpdateTool {
    name: ToolName,
    description: &'static str,
    input_schema: Arc<Value>,
    deps: MemoryToolDeps,
}

impl std::fmt::Debug for MemoryUpdateTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryUpdateTool").finish_non_exhaustive()
    }
}

impl MemoryUpdateTool {
    #[must_use]
    pub fn new(deps: MemoryToolDeps) -> Self {
        let name = ToolName::try_from(UPDATE_TOOL_NAME).expect("valid tool name");
        let input_schema = Arc::new(json!({
            "type": "object",
            "required": ["handle", "content"],
            "properties": {
                "handle": { "type": "string", "pattern": "^M-[0-9]+$" },
                "content": { "type": "string", "minLength": 1, "maxLength": 4096 }
            },
            "additionalProperties": false,
        }));
        Self {
            name,
            description: UPDATE_DESCRIPTION,
            input_schema,
            deps,
        }
    }
}

#[async_trait]
impl Tool for MemoryUpdateTool {
    fn name(&self) -> &ToolName {
        &self.name
    }
    fn description(&self) -> &str {
        self.description
    }
    fn input_schema(&self) -> Arc<Value> {
        self.input_schema.clone()
    }
    async fn execute(&self, _input: Value) -> Result<String, ToolError> {
        Err(ToolError::InvalidInput(
            "memory_update requires per-call context; invoke via execute_with_ctx".into(),
        ))
    }
    async fn execute_with_ctx(
        &self,
        input: Value,
        ctx: &ToolCallContext,
    ) -> Result<String, ToolError> {
        let parsed: UpdateInput = serde_json::from_value(input)?;
        let agent = expect_agent(ctx)?;
        check_cap(&self.deps.counter, ctx.request_id)?;

        let handle = MemoryHandle::try_from(parsed.handle.as_str()).map_err(parse_to_tool_err)?;
        let memory_id = resolve_handle(&self.deps, ctx.session_id, agent, handle).await?;
        let content = MemoryContent::try_from(parsed.content).map_err(parse_to_tool_err)?;

        let outcome = self
            .deps
            .store
            .apply(MemoryMutation::Update {
                agent,
                target: memory_id,
                content,
                state: MemoryState::Tentative,
                source: MutationSource::Turn(ctx.request_id),
                operator_override: false,
            })
            .await
            .map_err(store_to_tool_err)?;

        let out = UpdateOutput {
            memory_id: outcome.memory_id,
            handle: handle.to_string(),
            state: MemoryState::Tentative.as_str(),
        };
        Ok(serde_json::to_string(&out)?)
    }
}

/// `memory_forget` tool — removes a memory from the materialized view.
pub struct MemoryForgetTool {
    name: ToolName,
    description: &'static str,
    input_schema: Arc<Value>,
    deps: MemoryToolDeps,
}

impl std::fmt::Debug for MemoryForgetTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryForgetTool").finish_non_exhaustive()
    }
}

impl MemoryForgetTool {
    #[must_use]
    pub fn new(deps: MemoryToolDeps) -> Self {
        let name = ToolName::try_from(FORGET_TOOL_NAME).expect("valid tool name");
        let input_schema = Arc::new(json!({
            "type": "object",
            "required": ["handle"],
            "properties": {
                "handle": { "type": "string", "pattern": "^M-[0-9]+$" }
            },
            "additionalProperties": false,
        }));
        Self {
            name,
            description: FORGET_DESCRIPTION,
            input_schema,
            deps,
        }
    }
}

#[async_trait]
impl Tool for MemoryForgetTool {
    fn name(&self) -> &ToolName {
        &self.name
    }
    fn description(&self) -> &str {
        self.description
    }
    fn input_schema(&self) -> Arc<Value> {
        self.input_schema.clone()
    }
    async fn execute(&self, _input: Value) -> Result<String, ToolError> {
        Err(ToolError::InvalidInput(
            "memory_forget requires per-call context; invoke via execute_with_ctx".into(),
        ))
    }
    async fn execute_with_ctx(
        &self,
        input: Value,
        ctx: &ToolCallContext,
    ) -> Result<String, ToolError> {
        let parsed: ForgetInput = serde_json::from_value(input)?;
        let agent = expect_agent(ctx)?;
        check_cap(&self.deps.counter, ctx.request_id)?;

        let handle = MemoryHandle::try_from(parsed.handle.as_str()).map_err(parse_to_tool_err)?;
        let memory_id = resolve_handle(&self.deps, ctx.session_id, agent, handle).await?;

        let outcome = self
            .deps
            .store
            .apply(MemoryMutation::Forget {
                agent,
                target: memory_id,
                source: MutationSource::Turn(ctx.request_id),
                operator_override: false,
            })
            .await
            .map_err(store_to_tool_err)?;

        let out = ForgetOutput {
            memory_id: outcome.memory_id,
            handle: handle.to_string(),
            status: "forgotten",
        };
        Ok(serde_json::to_string(&out)?)
    }
}

fn expect_agent(ctx: &ToolCallContext) -> Result<AgentId, ToolError> {
    ctx.viewer
        .agent_id()
        .ok_or_else(|| ToolError::Backend("memory tool invoked with non-agent viewer".into()))
}

fn check_cap(counter: &MutationCounter, request_id: PromptRequestId) -> Result<(), ToolError> {
    counter.try_increment(request_id).map(|_| ()).map_err(|e| {
        ToolError::InvalidInput(format!(
            "memory mutation cap exceeded for this turn (max {} mutations)",
            e.cap
        ))
    })
}

fn parse_to_tool_err(e: ParseError) -> ToolError {
    ToolError::InvalidInput(e.to_string())
}

fn store_to_tool_err(e: MemoryStoreError) -> ToolError {
    match e {
        MemoryStoreError::NotFound { .. }
        | MemoryStoreError::WrongAgent { .. }
        | MemoryStoreError::PinnedImmutable { .. }
        | MemoryStoreError::Parse(_) => ToolError::InvalidInput(e.to_string()),
        MemoryStoreError::Db(_) => ToolError::Backend(e.to_string()),
    }
}

/// Resolve a session-scoped `M-NN` handle to its underlying memory id.
///
/// Composes the section through the session cache if it is not already
/// loaded — this is the same path the renderer takes, so a session that
/// just rolled past TTL pays one cache reload, not an error.
async fn resolve_handle(
    deps: &MemoryToolDeps,
    session: SessionId,
    agent: AgentId,
    handle: MemoryHandle,
) -> Result<MemoryId, ToolError> {
    let store = deps.store.clone();
    let section = deps
        .session_cache
        .get_or_load(session, agent, || async move {
            let rows = store.list(agent).await.map_err(|e| {
                warn!(error = %e, relay.agent.id = %agent, "memory_tool.list.error");
                e
            })?;
            Ok::<_, MemoryStoreError>(compose_memory_section(&rows))
        })
        .await
        .map_err(store_to_tool_err)?;

    section
        .handles()
        .resolve(handle)
        .ok_or_else(|| {
            ToolError::InvalidInput(format!(
                "unknown memory handle {handle}; check the `## Memory` section in your system prompt for valid handles"
            ))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counter_caps_at_per_turn_limit() {
        let counter = MutationCounter::new();
        let req = PromptRequestId::new();
        for i in 1..=MAX_MEMORY_MUTATIONS_PER_TURN {
            assert_eq!(counter.try_increment(req).expect("under cap"), i);
        }
        assert!(counter.try_increment(req).is_err());
    }

    #[test]
    fn counter_is_per_request() {
        let counter = MutationCounter::new();
        let r1 = PromptRequestId::new();
        let r2 = PromptRequestId::new();
        for _ in 0..MAX_MEMORY_MUTATIONS_PER_TURN {
            counter.try_increment(r1).expect("r1 ok");
        }
        // r2's quota is fresh.
        counter.try_increment(r2).expect("r2 ok");
    }
}
