# Backend plan — chat UI surface

## Scope

Three new HTTP endpoints to support the chat UI. No new auth, no channels, no schema changes to existing tables (an in-memory cursor for stream resume is good enough — see G3).

Read these before starting:

- `src/runtime/response.rs` — `ResponseChunk` enum (wire shape; do not change).
- `src/runtime/pg_response.rs` — current per-request publish/subscribe.
- `migrations/00000000000004_multi_agent_comm.up.sql` — DAG/session schema.
- `src/tools/system/send_message.rs` — chunk routing in the multi-agent path.

## Endpoints

### G1 — `GET /threads`

List of human-initiated top-level threads for the channel feed.

Query basis: roots of the DAG — `prompt_requests` where `id = root_request_id` and `sender_kind = 'human'`.

Response (one row per thread):

```json
{
  "root_request_id": "uuid",
  "root_session_id": "uuid",
  "first_agent": { "id": "uuid", "name": "string" },
  "preview": "first 280 chars of the human prompt content",
  "reply_count": 12,
  "last_activity_at": "ts",
  "status": "pending|processing|done|failed",
  "created_at": "ts"
}
```

`reply_count` = `COUNT(*)` over `session_messages` joined to every session whose `root_request_id` matches the row.

Pagination: `?before=<ts>&limit=N`. Default `limit=50`, cap `limit=100`. Sorted by `last_activity_at DESC`.

### G2 — `GET /threads/{root_request_id}/messages`

Flat history of every `session_messages` row across every session in the DAG. Used by the FE on thread open + on SSE reconnect to dedup.

Response:

```json
[
  {
    "session_id": "uuid",
    "seq": 42,
    "sender":   { "kind": "human" } | { "kind": "agent", "agent_id": "uuid" } | { "kind": "system" },
    "receiver": { "kind": "human" } | { "kind": "agent", "agent_id": "uuid" },
    "body": <ChatMessage JSONB verbatim>,
    "created_at": "ts"
  }
]
```

Ordered by `(created_at, seq)`. Page size cap: 1000. Pagination via `?before_seq=&before_ts=&limit=`.

### G3 — `GET /threads/{root_request_id}/stream`

Live DAG-wide SSE. Fans in chunks from every `prompt_requests` row in the DAG.

**Mechanism**: one `LISTEN` connection per process. `pg_response.rs` emits a `pg_notify` on every chunk insert with payload `{request_id, root_request_id, chunk_seq}`. A new fan-in subscriber (`runtime/pg_thread_stream.rs`) demuxes by `root_request_id` into per-thread `tokio::sync::broadcast` channels. The HTTP handler subscribes a single broadcast receiver and forwards.

Per chunk emitted to the client:

- `event:` — existing chunk kind (`text` | `reasoning` | `tool_call` | `tool_result` | `agent_message` | `done` | `error` | `stalled`).
- `id:` — per-thread stream cursor (monotonic, in-memory; see resume).
- `data:`
  ```json
  {
    "request_id": "uuid",
    "from_agent": "uuid|null",
    "chunk_seq": 7,
    "chunk": <ResponseChunk JSONB>
  }
  ```

**Sequencing (settled)**: per-request `chunk_seq` is the only ordering guarantee. Cross-session arrival order on the stream is arbitrary — when agent A invokes B and C in parallel, their chunks interleave freely. The FE handles layout via the parent-child session linkage from G2 + the per-bubble `request_id` grouping; it does **not** need a global DAG-wide ordinal.

**Stream cursor**: per-thread in-memory counter. Lossy on process restart, which is fine — resume is best-effort. Full correctness comes from FE refetching G2 and deduping by `(request_id, chunk_seq)` on reconnect.

**Discovery of new child requests**: implicit. The first chunk for a new child request arrives on the same NOTIFY channel; the fan-in subscriber sees its `request_id` for the first time and forwards. No separate discovery.

**Terminal event**: when the DAG is quiescent (existing `DagBudget::quiescent` check), emit a synthetic stream-terminal event. The HTTP handler closes the SSE connection after sending it.

**Backpressure**: bounded broadcast channel per thread; on lag, send `stalled` and let the client reconnect (existing pattern in `pg_response.rs`).

## Implementation pointers

| Concern | Where |
|---|---|
| Route wiring | new `src/http/routes/threads.rs`, merged in `routes/mod.rs` |
| NOTIFY emit | extend `src/runtime/pg_response.rs` publish path |
| Fan-in subscriber | new `src/runtime/pg_thread_stream.rs`; owns one LISTEN connection, per-thread `broadcast` map |
| AppState | add `SharedThreadStream` to `src/http/state.rs` |
| Caps | per-thread broadcast channel size in `runtime/limits.rs` (named constant; document why) |

Follow CLAUDE.md throughout — newtype every id (no bare `Uuid` in new public surface), `tracing::instrument` every span (`thread.list`, `thread.history`, `thread.stream.subscribe`, `thread.notify.fan_in`), `thiserror` per module.

## Build order

1. G1 (`GET /threads`) — read-only, smallest surface. Tests via `#[sqlx::test]` seeding rows.
2. G2 (`GET /threads/{id}/messages`) — read-only join through DAG.
3. NOTIFY hook in `pg_response.rs` — single-line addition to the existing publish path.
4. Thread fan-in subscriber + broadcast channel.
5. G3 SSE handler over the broadcast channel.
6. Quiescent-terminal synthetic event + tests.

FE can work against G1 + G2 in parallel before G3 lands.

## Out of scope

- `channel_id` column (deferred — single hardcoded `#general` on the FE).
- Auth/users/DMs.
- Approval-flow surfacing for hooks.
- Token / latency stats chunks.
- Pagination for `GET /threads` beyond cursor `before=ts`.
