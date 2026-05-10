# Agent Memory

## 1. Feature

### 1.1 Purpose

Each agent in Relay has its own private memory that persists across sessions. Memory is the agent's distilled understanding of its own role, the peers and humans it works with, and the procedures it has learned — extracted from past conversations and held in a bounded, typed, journaled store.

Memory is the *summary* of experience, not the experience itself. Raw conversation logs continue to live in the messages table; memory holds only what the agent has decided is worth carrying forward.

The design serves one product goal: agents that behave like real employees — they accumulate knowledge, refine preferences, follow operator-set core values, and self-evolve over time without drifting away from their pinned identity.

### 1.2 What a memory is

A single memory row carries:

- **content** — one or two sentences. The memory itself.
- **kind** — exactly one of:
  - *Self* — identity, style, preferences ("I default to terse replies").
  - *Other* — beliefs about specific peers or humans ("translator is fast on European languages").
  - *Procedure* — learned how-tos ("for ambiguous deadlines, propose a date and ask to confirm").
  - *Open* — known unknowns ("I don't have a confident play for vague translation targets").
- **state** — exactly one of:
  - *core* — operator-pinned, immutable to the agent.
  - *validated* — confirmed by independent signals.
  - *held* — default accepted.
  - *tentative* — newly written, unverified.
- **pinned** — operator flag. Pinned memories cannot be updated or forgotten by the agent.
- **provenance** — the turn that produced this memory.
- **embedding** — vector for similarity retrieval.
- **timestamps** — `created_at`, `last_validated_at`, `last_accessed_at`.
- **access_count** — read counter. Frequently-read memories survive decay longer.

The hidden underlying confidence is a number; the agent only ever sees the qualitative state. This avoids calibration drift — agents reason about "tentative vs validated" reliably; they reason about "0.42 vs 0.61" poorly.

What memory does NOT store: identity facts that exist in the agent registry (peer ids, capabilities, role descriptions), transient session state, or raw episodic content. Facts come from the registry, episodic content from the message log, beliefs and learnings from memory.

### 1.3 How memory enters the system prompt

Memory is composed into the system prompt after the role block, in two layers. Both layers are assembled at session start and frozen for the session's lifetime — within a session, memory is read-only, which keeps the cached prefix stable across turns.

System prompt composition for any turn:

```
core              (binary constant, communication protocol — same for every agent)
role              (per-agent column on the agents record — operator-set identity)
memory:
  stable layer    (pinned + Self-kind, token-capped)
  contextual      (top-K embedding-retrieved Other/Procedure/Open, against
                   the session's opening context)
```

The user prompt is unchanged — it remains whatever the human or peer agent sent. Memory does not enter the user prompt as ambient data.

#### Render shape

Memory renders as a kind-grouped bulleted list with short handles and state tags:

```
## Memory

### Self
- [M-12, validated] You default to terse replies unless asked otherwise.
- [M-18, held] You ask one focused clarifying question rather than several.

### Other
- [M-31, held] translator is fast on European languages; ask for context with idioms.
- [M-44, tentative] marketing-lead asks vague questions — ask back before delegating.

### Procedure
- [M-07, validated] For ambiguous deadlines, propose a concrete date and ask to confirm.

### Open
- [M-52, tentative] You don't have a confident play for handling vague translation targets.
```

Three properties this gets right:

- The state tag is visible to the agent so it can adjust trust without seeing a number.
- Short handles (M-NN) — not raw UUIDs — are stable for the session and cheap to dereference. The agent uses these in tool calls.
- Kind-grouped structure teaches the agent how to relate to each kind by observation.

### 1.4 How memory is read mid-session

If the contextual layer missed something the agent needs, it calls the `recall` tool:

```
recall(query: string, kind?: MemoryKind, limit?: u8)
```

The tool runs an embedding query against the agent's memory store and returns matching rows as a tool result. The result lands in the conversation as a normal tool-call / tool-result pair. Per-turn caps prevent spam.

`recall` is for nuance, not load-bearing knowledge. Anything the agent must not lose belongs in the stable layer (pinned + Self).

### 1.5 How memory is written

The agent has three memory mutation tools:

```
memory_write(kind, content)
memory_update(handle, content)
memory_forget(handle)
```

These tools are registered once and available in every turn the agent runs. There is no special "memory mode" or per-context tool surface. What varies between contexts is the *job* the agent is doing on that turn:

- **Normal turn** — the agent uses memory tools only when the conversation explicitly asks for it. "Remember I prefer tabs over spaces" → `memory_write`. "Your memory M-12 is wrong, the deadline is Tuesday" → `memory_update`. The agent does NOT write memory based on weak or implicit signals during a normal turn — implicit-signal capture is reflection's job, not the current turn's.
- **Reflection turn** — autonomous self-curation. The agent reviews the conversation since the last checkpoint and decides what to write, update, or forget. Hard cap on mutations per reflection.
- **Librarian-resolution turn** — focused on a single contradiction pair detected by the librarian. The agent decides which memory to keep, update, or forget.

All three contexts call the same tools, write through the same journal-append-plus-upsert path, and produce the same shape of mutation. Only the prompt's framing of the agent's job differs.

### 1.6 Reflection turns — autonomous self-curation

A reflection turn is what the agent does to consolidate after a conversation. It runs in the background, has no audience, and produces only memory mutations.

#### When reflection runs

A scheduled sweep finds sessions where:

- the time since the last turn exceeds a configurable idle timeout, AND
- there are turns past the latest reflection checkpoint for that (agent, session).

Those sessions qualify for reflection. Sessions are not assumed to ever "end" — a human may resume tomorrow. Reflection runs whenever a session has been idle long enough and has unprocessed turns.

#### Checkpointing

Each (agent, session) carries a `reflection_checkpoint` row that records the last turn id processed by the most recent reflection. The next reflection processes only turns after that point. When reflection completes, the checkpoint advances atomically with the journal writes.

This makes reflection idempotent across session resumption. A session that gets reflected on Monday and resumes Wednesday will produce a second reflection on Wednesday's idle timeout, processing only the Wednesday turns.

#### Reflection's inputs

A reflection turn sees:

- The agent's role block (identity preserved across normal and reflection turns — the translator reflecting is still the translator).
- A reflection-specific core prompt (see §1.6 composition below) that defines the reflection protocol, output format, and write quotas.
- The new turns since the last checkpoint.
- The same memory layers a normal turn would see, retrieved against the new turns rather than the session opener — stable layer + contextual top-K. This is bounded by the same token cap as a normal turn; reflection does NOT see the agent's full memory state.

Reflection does NOT see the prior reflection turn's reasoning or tool calls. The previous reflection's *outputs* are already merged into the agent's current memory state and visible there; its *reasoning* stays private. This is the structural anti-self-reinforcement guard.

#### System prompt composition for a reflection turn

```
reflection_core   (binary constant — replaces the normal core for reflection)
role              (same per-agent column as normal — identity preserved)
[optional]
reflection_role   (optional per-agent column — role-specific reflection guidance)
memory:
  stable layer    (same assembly as normal turns)
  contextual      (top-K against the new turns being reflected on)
```

#### Output

Structured tool calls only — `memory_write`, `memory_update`, `memory_forget`. No prose, no audience. A hard cap limits total mutations per reflection so dreams cannot produce 30 writes a night and bloat the journal with noise.

### 1.7 Lifecycle — how memories age and change state

A new memory enters the system as `tentative`.

Promotion:

- *tentative → held* — the memory survives one librarian sweep without being merged out and is not contradicted within a configurable age threshold.
- *held → validated* — an *independent* signal confirms the memory: an independent re-write in a different session, an external confirmation event, or an operator endorsement.

Demotion:

- *validated → held* — extended non-access (decay).
- *any non-pinned → forget* — confidence floor crossed plus age threshold plus low access count.

Pinned memories are exempt from every demotion path. A signal that contradicts a pinned memory is journaled for operator review rather than changing the state.

The validation clock — `last_validated_at` — advances ONLY on independent signals:

- Cross-session re-write (the same content emerging in a separate session, not a self-citation in a dream).
- External confirmation (web search, peer agent, human).
- Operator endorsement via `manager_note`.

Mere reading into context does NOT advance validation. A memory that's read constantly but never re-validated lives (because of the access counter) but does not promote (because validation requires independent evidence).

### 1.8 Librarian — mechanical maintenance and contradiction resolution

The librarian runs on a schedule, per agent. It has two phases:

#### Mechanical sweep (no LLM)

Pure SQL plus embedding operations:

- *Dedup* — pairs with cosine similarity above a threshold are merged, keeping the higher-state and older-provenance copy.
- *Decay* — `last_validated_at` thresholds drop confidence; state demotes when buckets cross.
- *Eviction* — when an agent is over its memory quota, lowest-score non-pinned memories are forgotten (forget event in the journal).
- *Contradiction detection* — pairs with high embedding similarity but textually opposed signals get written as `contradiction_events` rows. Detection only — no resolution.

#### Resolution turn (LLM, focused)

If the mechanical sweep produced unresolved contradiction events for an agent, a focused turn runs once per pair:

- The agent's role block (identity preserved — resolution speaks with the agent's voice).
- A resolution-specific core prompt — bounded, single-job: "given memory A and memory B (which contradict), keep, update, or forget."
- Inputs: the two memories, their provenance, no other memory state, no conversation context.
- Output: structured tool calls (`memory_update` or `memory_forget`) or "no action — both correct in different contexts," which marks the contradiction event resolved without a write.

This isolation is deliberate. The agent's job for the resolution turn is solving one contradiction. Bringing in conversation history, recent memories, or other open contradictions would add noise and dilute the focus.

### 1.9 Operator authority

The operator (today: the single human responsible for the deployment) has read, pin, and revert authority on the journal:

- `manager_note(agent_id, content, kind, pinned)` — direct memory write, attributed to the operator. Notes can be written at `validated` or pinned at `core` immediately, since operator endorsement is one of the validation signals.
- *Pin / unpin* — toggles the pinned flag on a memory, raising or lowering its protection.
- *Revert* — appending an inverse journal event undoes any past mutation. The materialized view recomputes from the journal.

There is no inline approval — operator audit happens asynchronously. This preserves agent autonomy. The model is "agent acts, operator audits later," like a manager reviewing an employee's work.

### 1.10 Drift defenses

The structural separations above kill the catastrophic drift modes. Smaller defenses handle everyday belief noise:

- *Facts vs beliefs* — registry holds facts (peer ids, capabilities, role descriptions); memory holds beliefs only. Catastrophic facts cannot drift because they are not stored as memory.
- *Memory mutates only on explicit job* — normal turns mutate only on explicit conversational request; otherwise reflection or resolution does it. No implicit mid-turn writes.
- *Nuance by default* — every agent-written memory starts `tentative`. Promotion requires independent validation or operator pinning.
- *Operator pin* — invariants the agent cannot edit.
- *Decay* — `last_validated_at` ages mechanically, no LLM cost.
- *Quarantine* — `tentative` cannot promote until a librarian sweep confirms it survives dedup and contradiction.
- *Anti-self-reinforcement* — reflection sees user-facing conversation, not its own previous reasoning.
- *Independent-signal-only validation* — self-citation in a dream does not advance the validation clock.
- *Journal as truth* — every mutation is replayable and revertable.
- *Per-agent quota* — hard cap on memory rows; eviction forces real curation rather than hoarding.

### 1.11 Boundaries

- Memory is per-agent and private. No cross-agent reads.
- New agents start with empty memory. Cloning is a future operator action, not a default.
- Memory does not duplicate registry data or session messages — it summarizes only.

---

## 2. Implementation plan

The plan is delivered as a sequence of phases for clarity, but the entire subsystem ships in a single PR. Each phase below is a coherent slice of the diff; they are not separately deployable. The single-PR constraint forces the design to remain consistent across phases — no transitional shims, no half-built bridges.

The plan is built around one general mechanism per concern. There are no per-context special cases:

- One read path (stable + contextual layers, frozen at session start).
- One write path (tools → journal → materialized view).
- One tool surface (`recall`, `memory_write`, `memory_update`, `memory_forget`).
- One LLM entrypoint (the existing prompt queue → worker → agent), generalized to carry three job kinds (normal / reflection / resolution).
- One lifecycle (tentative → held → validated, with decay and forget).
- One operator surface (manager_note, pin/unpin, revert through the journal).
- Two provider traits side by side: the existing `LlmProvider` for chat, a sibling `EmbeddingProvider` for vector embeddings. Same SDK family, separately configured.

### 2.1 Phase 1 — Storage foundation

Build the persistent storage and the single transactional path through which all memory mutations flow.

Tables:

- `memory_events` — append-only journal. Fields: `id`, `agent_id`, `mutation` (`write` | `update` | `forget`), `target_memory_id`, `content_before`, `content_after`, `source` (`turn_id` | `operator` | `librarian`), `created_at`. The journal is the source of truth.
- `agent_memories` — materialized view derived from the journal. Fields: `id`, `agent_id`, `kind`, `content`, `state`, `pinned`, `source_turn`, `embedding`, `created_at`, `last_validated_at`, `last_accessed_at`, `access_count`. Fast to read; always rebuildable from the journal.
- `contradiction_events` — librarian-detected pairs awaiting resolution. Fields: `id`, `agent_id`, `memory_a`, `memory_b`, `reason`, `created_at`, `resolved_at`, `resolution_event_id`.
- `reflection_checkpoints` — per (agent, session). Fields: `agent_id`, `session_id`, `last_turn_id`, `reflection_event_id`, `created_at`.

Existing-table change — `prompt_requests` gains two columns to carry the job kind:

- `kind` (enum: `Normal` | `Reflection` | `Resolution`, default `Normal`). All existing rows backfill to `Normal`.
- `kind_payload` (nullable JSONB). Carries kind-specific metadata: nothing for `Normal`; `{ session_id, since_turn_id }` for `Reflection`; `{ contradiction_event_id }` for `Resolution`.

This generalizes the prompt queue from "queue of prompts" to "queue of agent jobs" without forking the worker pool, lease semantics, or observability. See §2.4 and §2.7 for how each kind dispatches.

Mutation function:

A single function appends to `memory_events` and upserts the materialized row in one transaction. Every memory write in the entire system goes through this function. There is no path that bypasses it.

Newtypes (per CLAUDE.md §1):

- `MemoryId` (UUID newtype).
- `MemoryKind` (enum: Self, Other, Procedure, Open).
- `MemoryState` (enum: Core, Validated, Held, Tentative).
- `MemoryContent` (validated string newtype with size cap).
- `ContradictionEventId`, `ReflectionCheckpointId` (UUID newtypes).
- `MemoryHandle` (the M-NN form, parses bidirectionally).
- `RequestKind` (enum: Normal, Reflection, Resolution) — lives in `runtime::types` next to `RequestStatus`.

Traits:

- `MemoryStore` — the read/write surface. Has `PgMemoryStore` as the concrete implementation.
- `MemoryStoreError` — the module's error enum (per CLAUDE.md §12).

Migrations: paired up/down per CLAUDE.md §14. Adds the pgvector extension if missing. The `prompt_requests` migration backfills `kind = 'Normal'` for all existing rows before applying the `NOT NULL` constraint.

Tests cover journal append correctness, materialized-view consistency, replay rebuild from events, concurrent-mutation safety, and `prompt_requests` migration safety (existing rows continue to dispatch as `Normal`).

### 2.2 Phase 2 — Memory in the system prompt

Extend `AgentMemory` (the existing layer that composes core + role) to also compose a memory section.

The composer accepts an `agent_id` and a session-opening context, and produces:

- A stable layer: pinned + all `Self`-kind memories for the agent, sorted by state and recency, trimmed to a token budget.
- A contextual layer: top-K embedding-retrieved from `Other` / `Procedure` / `Open`, weighted by similarity × recency × state, filtered to exclude `tentative` rows below a confidence floor.

Both layers are rendered as a kind-grouped bulleted list with short handles. The composer maintains a per-session handle map (M-NN ↔ UUID) so the agent's tool calls can be resolved back to UUIDs.

The assembly runs once at session start. There is no mid-session refresh path.

Tests cover: stable-layer ordering and trimming, contextual retrieval determinism, handle map round-trip, cap enforcement, frozen-during-session invariant.

### 2.3 Phase 3 — Memory tools

Register four tools in the existing system tool registry (`src/tools/system/`):

- `recall(query, kind?, limit?)` — embedding query against the caller's memory; returns matching rows with handles, kinds, states, and content. Updates `last_accessed_at` and `access_count` on returned rows. Per-turn rate limit.
- `memory_write(kind, content)` — creates a new memory in `tentative` state. Provenance set to the calling turn.
- `memory_update(handle, content)` — updates an existing memory's content. Resets state to `tentative` (a content change is, by definition, unverified again).
- `memory_forget(handle)` — appends a forget event; the materialized row is removed.

All four tools resolve `agent_id` from the calling agent's identity (carried in `ToolCallContext`). All three mutation tools route through the phase-1 transactional function. Pinned memories reject `update` and `forget` from agent calls — only operator paths can mutate pinned rows.

Per-turn caps are enforced uniformly: a single turn cannot produce more than N total memory mutations (configurable, default low).

Tests cover: tool registration, schema validation, tool result shapes, agent-vs-operator pinned protection, rate limits, handle-to-UUID resolution.

### 2.4 Phase 4 — Reflection turn

Reflection flows through the same prompt queue and worker pool as normal turns. The worker dispatches on the new `kind` column from §2.1.

Trigger: a `ReflectionScheduler` background task (alongside the existing `McpRefresher` pattern) polls the DB on a configurable cadence. It finds `(agent_id, session_id)` pairs where the time since the last turn exceeds the idle timeout AND there are turns past the latest `reflection_checkpoints` row. For each qualifying pair it calls `PromptQueue::enqueue` with `kind = Reflection` and `kind_payload = { session_id, since_turn_id }`. The scheduler does not talk to the LLM; it only enqueues.

Worker dispatch: when a worker claims a session whose row is `kind = Reflection`, it calls `agent.reflect(session_id, since_turn_id)` instead of the existing `agent.reply_batch`. All other worker machinery (lease heartbeats, idempotency, retry on lease expiry, span propagation, cancellation, MAX_TURN_DURATION timeout) applies unchanged.

Concurrency: per-agent serialization at the queue level. Two `Reflection` (or `Reflection` + `Resolution`) jobs for the same agent never run in parallel. The existing claim semantics already serialize per session; per-agent serialization layers on top via a SELECT predicate that skips a row whose `agent_id` already has an in-flight job of any memory-mutating kind. Different agents reflect concurrently with no constraint.

Reflection turn assembly (built inside `agent.reflect`):

- System prompt: `reflection_core` (new binary constant — placeholder content; the actual prompt engineering is a separate task) + the agent's `role` + optional `reflection_role` (new nullable column on `agents`) + memory layers retrieved against the new turns.
- User prompt: the new turns since the last checkpoint, with a short header naming the reflection task.
- Tools available: `memory_write`, `memory_update`, `memory_forget`. NOT available: `send_message`, `recall` (the contextual layer is already retrieved against the new turns, so `recall` would be redundant), or any non-memory system tool.
- Output sink: nothing. Reflection's output is journal writes via tool calls; no `ResponseChunk` is emitted, no SSE stream is touched.
- Hard cap on total mutations per reflection.

After the reflection turn completes, advance the checkpoint atomically with the journal writes — both happen in one transaction.

Anti-self-reinforcement: reflection inputs include the user-facing conversation only. The previous reflection turn's tool calls and reasoning are NOT included. The previous reflection's *outputs* are visible only via the agent's current memory state (which is already there for any turn).

Tests cover: scheduler enqueue conditions, worker dispatch on `kind = Reflection`, per-agent serialization, checkpoint advancement, cap enforcement, anti-recursion (a `Reflection` job cannot enqueue another `Reflection` job — the agent's tool surface in this kind has no path to do so), input composition (no prior reasoning leakage).

### 2.5 Phase 5 — Lifecycle

Implement the state machine inside the phase-1 mutation function.

Rules:

- Writes default to `Tentative`.
- Updates reset state to `Tentative` (content changed → unverified again). Pinned-memory updates from operator paths stay at the operator-specified state.
- Forgets are immediate from the materialized view; the journal retains the row.
- Promotion (`Tentative → Held` and `Held → Validated`) is driven by:
  - Independent re-write detection (cross-session content match by embedding similarity, where the new write's source is a different session than any prior row of that content).
  - External confirmation events (recorded as a separate `validation_events` table written by the dispute-validation paths in normal turns — when an agent's `recall` plus `web_search` plus reply confirms a memory, the system records a validation event).
  - Operator endorsement via `manager_note` with explicit state.
- Demotion (`Validated → Held`) on `last_validated_at` aging past threshold.
- Forget on confidence floor + age + low access count.
- Pinned memories are exempt from every transition except operator-driven.

Decay runs as a scheduled job, mechanically demoting rows whose `last_validated_at` has aged past thresholds.

Tests cover: each transition, pinned immunity, validation-event recording, decay timing, self-citation rejection (a re-write within the same session from an existing memory's context does not trigger validation).

### 2.6 Phase 6 — Librarian mechanical sweep

Build the no-LLM librarian. Runs as a `LibrarianScheduler` background task on a schedule (e.g., nightly) per agent.

Operations, in order:

1. *Dedup* — for each pair of the agent's memories with cosine similarity above threshold, merge: keep the higher-state, older-provenance copy; emit a forget event for the other; carry over the access counts.
2. *Decay* — apply phase-5 decay rules.
3. *Eviction* — if the agent is over quota, sort non-pinned memories by score (`state × recency × log(access_count)`), forget the bottom until under quota.
4. *Contradiction detection* — for each pair with high embedding similarity but textually opposed signals (heuristic: high cosine plus opposing keywords or negation patterns), insert a `contradiction_events` row.

All actions write to the journal with `source = librarian`.

Tests cover: dedup correctness with state-preference tiebreaking, decay determinism, eviction respects quota and pinning, contradiction detection on canned cases.

### 2.7 Phase 7 — Librarian-resolution turn

After the mechanical sweep in §2.6 completes, the `LibrarianScheduler` enqueues one job per unresolved `contradiction_events` row via `PromptQueue::enqueue` with `kind = Resolution` and `kind_payload = { contradiction_event_id }`.

Worker dispatch: when a worker claims a `kind = Resolution` row, it calls `agent.resolve_contradiction(contradiction_event_id)` instead of `agent.reply_batch` or `agent.reflect`. Same lease, same observability, same retry — the only difference from a normal turn is the dispatched method.

Composition (built inside `agent.resolve_contradiction`):

- System prompt: `resolution_core` (new binary constant — placeholder content) + the agent's `role`. NO `reflection_role`, NO general memory layers, NO conversation context.
- User prompt: the two memories (handle, kind, state, content, provenance) and the librarian's reason for flagging the pair.
- Tools available: `memory_update`, `memory_forget`, plus a `resolution_no_action(reason)` tool that marks the contradiction event resolved without mutating either memory.
- Output sink: nothing — same as reflection.
- Hard cap: one tool call.

Per-agent serialization shared with phase 4 — reflection and resolution for the same agent never overlap (enforced by the same per-agent claim predicate).

After the turn, mark the contradiction event resolved with the link to the resulting journal event (or the `no_action` reason).

Tests cover: scheduler enqueue per unresolved event, worker dispatch on `kind = Resolution`, turn composition (no extraneous context), one-call cap, resolution-event linking, serialization with reflection.

### 2.8 Phase 8 — Operator audit and revert

Add HTTP routes:

- `GET /agents/:id/memory` — list current memories for the agent (read of the materialized view).
- `GET /agents/:id/memory/events` — list journal events with filters by source, mutation kind, time range.
- `POST /agents/:id/memory/notes` — `manager_note` API. Accepts content, kind, optional pinned flag, optional state override.
- `POST /agents/:id/memory/:handle/pin` and `/unpin`.
- `POST /agents/:id/memory/events/:event_id/revert` — appends an inverse event; the materialized view updates accordingly.

All operator routes write to the journal with `source = operator`. There is no path that bypasses the journal.

Tests cover: revert correctness, pin/unpin transitions, manager_note seeding, journal completeness across operator and agent paths.

### 2.9 Phase 9 — Embedding provider

Embeddings are a sibling concern to LLM chat: same SDK family (`async-openai` works against any OpenAI-compatible endpoint), different surface, separately configured. The existing `LlmProvider` trait stays untouched — Anthropic does not provide embeddings, and bolting an unused method onto a trait would force unimplemented stubs.

New trait in `src/provider/traits.rs`:

```rust
#[async_trait]
pub trait EmbeddingProvider: fmt::Debug + Send + Sync {
    fn name(&self) -> &'static str;
    fn dimensions(&self) -> usize;
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, ProviderError>;
}

pub type SharedEmbeddingProvider = Arc<dyn EmbeddingProvider>;
```

`ProviderError` is reused as-is — its existing variants (`Unauthorized`, `RateLimited`, `Transient`, `Decode`, `Transport`, `InvalidRequest`) cover embedding failures without modification.

Concrete implementation: `OpenAiEmbeddingProvider` in `src/provider/openai/embedding.rs`, sibling to the existing `client.rs`. Wraps `Client<OpenAIConfig>` exactly like `OpenAiProvider`, calls `client.embeddings().create(...)`, and reuses the `map_runtime_error` helper (lifted to `src/provider/openai/mod.rs` so both `client.rs` and `embedding.rs` share it).

Configuration — three new env vars, independent of the existing chat-side `OPENAI_API_KEY` / `OPENAI_BASE_URL`:

- `EMBEDDING_API_KEY` (required).
- `EMBEDDING_BASE_URL` (optional, defaults to OpenAI's public base).
- `EMBEDDING_MODEL` (e.g., `text-embedding-3-small`).

The decoupling is deliberate: today chat goes to DeepSeek and embeddings to OpenAI; tomorrow either side can move without touching the other. If both ever point at the same provider, set the same base URL — no schema or code change.

Composition root (`src/app.rs::Collaborators::new`) builds both providers in parallel:

```rust
let provider: SharedProvider = build_llm_provider(settings)?;
let embeddings: SharedEmbeddingProvider = build_embedding_provider(settings)?;
```

The embedding handle is threaded into:

- `MemoryStore` — embeds `content` on every `memory_write` and `memory_update`, stores the vector in `agent_memories.embedding`.
- The retrieval functions in §2.2 (contextual layer at session start) and §2.3 (`recall` tool) — embeds the query text before the cosine search.
- The librarian's mechanical sweep in §2.6 — already has the per-row embeddings, so no extra calls; uses them for dedup and contradiction detection.

Failure handling: embedding failures during `memory_write` / `memory_update` propagate as `MemoryError::Provider` and abort the mutation (no orphaned rows without embeddings). Failures during retrieval degrade gracefully — the contextual layer renders empty for that session and a `relay.memory.retrieval.degraded` event is emitted; the stable layer still loads.

Tests cover: embedding dimensions match the column dimension, batched embed for librarian sweeps, error mapping, configuration parsing, mutation-aborts-on-embed-failure semantics.

### 2.10 Cross-cutting concerns

- *Tracing* (per CLAUDE.md §2): every mutation, retrieval, reflection, and resolution turn opens a span. Custom attributes use `relay.memory.*` (`relay.memory.id`, `relay.memory.kind`, `relay.memory.state`, `relay.memory.source`). The new `kind` column on `prompt_requests` rides on the existing claim/dispatch span as `relay.request.kind`.
- *Limits* (per CLAUDE.md §5): per-agent memory row quota, per-turn mutation cap, per-reflection mutation cap, recall result cap, content size cap, embedding query timeout, scheduler poll interval bounds. All in `src/memory/limits.rs`.
- *Errors* (per CLAUDE.md §12): one `MemoryError` enum at the module boundary covering store, lifecycle, embedding-provider, and tool-resolution failures.
- *Tests* (per CLAUDE.md §3): TDD throughout; integration tests use `#[sqlx::test]` against a real Postgres; no mocks for the store. The embedding provider is mocked at the trait level for unit tests; real embeddings only in the dedicated provider integration test. 100% coverage on the lifecycle state machine.

The memory subsystem ships when all phases are implemented and all gates green per CLAUDE.md §3 — `cargo fmt`, `cargo clippy -D warnings`, `cargo test`, and the e2e harness for the agent surface — in a single commit.
