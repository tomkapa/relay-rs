# Frontend plan — chat UI

## Stack

| | |
|---|---|
| Toolchain | **Bun** — install, dev server (`bun --hot`), bundler (`bun build`), test runner |
| UI | React 18 |
| Routing | TanStack Router |
| Server-state cache | TanStack Query |
| Live state | Zustand |
| Styling | Tailwind |
| Hosting | static `dist/` served by axum's `tower-http::ServeDir` |

One toolchain. No Vite, no Webpack, no Node.

## Backend mental model

Three concepts the FE must internalise. **Read the listed files** rather than paraphrasing this doc:

1. **Thread = DAG**, anchored by `root_request_id`. See `migrations/00000000000004_multi_agent_comm.up.sql`.
2. **Session = canonical 2-party pair** within a DAG. The FE never invents `session_id`; it always echoes one back from `POST /prompts`. See `src/types/participant.rs`.
3. **Plain assistant `text` chunks are private**; only `agent_message` is human-deliverable content. Reasoning + tool_call/tool_result ride alongside an `agent_message` as collapsible metadata. See top-of-file comment in `src/tools/system/send_message.rs`.

`ResponseChunk` variants and JSON shapes: `src/runtime/response.rs`.

## API contract

New endpoints (full spec in `doc/backend_plan.md`):

- `GET /threads` — channel feed list
- `GET /threads/{root_request_id}/messages` — thread history
- `GET /threads/{root_request_id}/stream` — live DAG-wide SSE

Existing endpoints used as-is:

| | Source |
|---|---|
| `POST /prompts` | `src/http/routes/prompts.rs` |
| `GET /agents` | `src/http/routes/agents.rs` |
| `POST /requests/{id}/cancel` | `src/http/routes/prompts.rs` |

## SSE handling

One `EventSource` per open thread, opened against G3.

**Bubble grouping**: bucket chunks by `request_id` from the data envelope. Each `request_id` is one bubble. Within a bucket, chunks order by `chunk_seq` (per-request, monotonic). **Across buckets, arrival order is arbitrary** — when agent A asks B and C in parallel, B's and C's chunks interleave; render bubbles independently. Layout the bubble graph via parent-child session linkage from G2.

**Render rules**:

| `chunk.kind` | UI |
|---|---|
| `text` | drop (private) |
| `reasoning` | append to bubble's reasoning section |
| `tool_call` | open row in bubble's tool-call timeline |
| `tool_result` | match by `call_id`, populate result + duration |
| `agent_message` | bubble's primary content; `from_agent` + `chunk.from`/`receiver` give the addressee chip |
| `done` | close bubble; freeze meta (tool count, etc.) |
| `error` | close bubble in error state with reason |
| `stalled` | force reconnect |

**Reconnect**: `Last-Event-ID` is best-effort (backend's stream cursor is in-memory). On open or reconnect, refetch G2 and dedup all live + replayed chunks by `(request_id, chunk_seq)` against the in-memory cache.

## State

| Bucket | Tool | Keys |
|---|---|---|
| Server cache | TanStack Query | `['threads']`, `['threads', rootId, 'messages']`, `['agents']` |
| Live stream | Zustand | `Map<rootRequestId, ThreadStreamState>` — per-`request_id` bubbles, `Set<(request_id, chunk_seq)>` dedup, status |
| UI | local component state | composer text, expand/collapse, currently-open thread |

## Submit flow

**Top-level**: `POST /prompts` with no `session_id`. Parse leading `@<name>` against the cached `GET /agents` → resolve `agent_id`; omit if no `@` (uses default agent). Generate UUIDv7 `idempotency_key`, retry-safe. On success, open EventSource on G3 for the returned `root_request_id`.

**Thread reply**: `POST /prompts` with the **root session_id** (the human↔topAgent session, returned originally). `agent_id` is ignored on existing sessions — receiver is preserved (see `prompts.rs`).

## Build order

Backend ships G1 + G2 first; FE can build steps 1–6 against those. G3 unblocks 3+.

1. Bootstrap: `bun init`, Tailwind, Router, Query. Wire `axum::ServeDir` for `dist/`. Page that lists `GET /agents` — verifies dev loop end-to-end.
2. Channel feed (G1, read-only).
3. Submit + open G3 SSE; render bubbles per-`request_id` with raw chunk JSON. Verifies the live pipeline.
4. Bubble UI: reasoning collapsible, tool-call timeline.
5. Thread history (G2): paint past messages on open, reconcile against incoming SSE chunks (dedup by `(request_id, chunk_seq)`).
6. Reply-in-thread (echo `session_id`).
7. Cancel.
8. Reconnect + resume (G2 refetch + dedup).
9. DAG-budget terminal rendering (decode `error` chunk reason).

Each step shippable on its own.

## Out of scope

Multi-channel, auth/users/DMs, attachments, emoji/reactions, approval flow UI, agent CRUD UI, tokens/latency badge.
