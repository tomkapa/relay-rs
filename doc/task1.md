Final architecture — in-process, Postgres-ready

Single Rust binary running tokio. Exposes HTTP and runs the agent worker pool in the same process. Designed so the worker pool can move to a separate binary the day storage moves to Postgres — code split is one trait swap, not a rewrite.

### Shape

A user owns sessions. A session owns conversation history. The user POSTs a prompt with optional `session_id` (server creates one if absent) and an `Idempotency-Key` header. Server inserts a `PromptRequest` row, returns `request_id`, and the client opens an SSE stream on `GET /requests/:id/stream` to watch the answer arrive. Behind the scenes, a worker picks up the prompt, runs the agent loop, streams chunks back through the SSE stream, persists final state, releases the lease.

Sessions are single-owner — one user, never shared — so multiple pending prompts on the same session are by construction consistent and get processed as one turn.

### HTTP surface

- `POST /sessions` → `{ session_id }`. Creates an empty session bound to the authenticated user.
- `POST /prompts` `{ session_id, content, idempotency_key }` → `{ request_id, status }`. Inserts into the queue. Retries with the same `idempotency_key` return the original `request_id` instead of inserting again.
- `GET /requests/:id/stream` → `text/event-stream`. Subscribes to chunks for that request. If the turn already finished, replays from storage and closes. If still running, pipes live chunks plus already-buffered earlier chunks (no missing prefix). On reconnect, client passes `Last-Event-ID` (the last `seq` it saw) and the handler resumes from there.
- `POST /requests/:id/cancel` → marks `cancellation_requested = true`. Worker honors at the next turn boundary.

### Storage — traits today, in-mem impls now

Three traits hide everything mutable. Each has an `InMemory*` impl behind it now and a `Pg*` impl later. The agent loop, worker pool, and SSE handlers depend only on the traits.

- `PromptQueue` — `enqueue(NewPromptRequest) -> PromptRequestId`, `claim_next_session(WorkerId) -> Option<ClaimedSession>`, `mark_done(ids, lease)`, `mark_failed(ids, err, lease)`, `request_cancellation(id)`. `claim_next_session` does claim-and-drain atomically and returns the snapshot of pending prompts plus a `LeaseToken` carrying `turn_seq`.
- `LeaseManager` — `heartbeat(lease)`, `release(lease)`. The `LeaseToken` is opaque; in-mem it's `(SessionId, turn_seq, Arc<…>)`, Pg it's the same with a `leased_until`. Drop logs a warning if not released cleanly.
- `ResponseSink` (publish side) and `ResponseSource` (subscribe side). Worker calls `sink.publish(request_id, chunk)`. SSE handler calls `source.subscribe(request_id) -> Stream<Chunk>`. In-mem: a `DashMap<PromptRequestId, broadcast::Sender<ResponseChunk>>`. Bounded buffer (capped lag = drop = SSE handler reads from the chunks log to catch up).

`SessionStore` (already in the codebase) keeps history and stays as-is.

Time enters every impl through the existing `Clock` trait — never `Instant::now` directly. Tests run on `TestClock` with `tokio::time::pause()` so lease expiry, attempts, and SSE timing are all deterministic.

### Worker loop

A bounded `JoinSet` of N worker tasks, sized at startup from config. Each worker:

```
loop until shutdown:
  match claim_next_session(worker_id):
    Some(claim):
      spawn lease-heartbeat task (extends every TTL/3, dies on drop)
      result = run_turn(claim)            // wrapped in tokio::time::timeout(MAX_TURN)
      match result:
        Ok(_)              -> mark_done(claim.prompt_ids, &claim.lease)
        Err(cancelled)     -> mark_failed(... reason=Cancelled, &claim.lease)
        Err(timeout|model) -> mark_failed(... reason=…, &claim.lease)
      release(claim.lease)
    None:
      sleep(1s)             // flat poll, no backoff, no notify yet
```

`run_turn` snapshots history from `SessionStore`, appends the drained prompts as a single `ChatMessage::User` containing `Vec<UserContent::Text>` (safe because of single-user-per-session), runs the model with streaming, publishes chunks via `ResponseSink::publish` as they arrive, appends the assistant message and any tool calls/results to history at the end. Cancellation flag is checked once before the turn starts and once after it ends — not on every await. CPU-cheap, predictable; the user gets to type "continue" if they stopped mid-thought.

### Lease + fencing

`session_leases` maps `session_id → (leased_by, leased_until, turn_seq)`. A worker claims by acquiring the lease and incrementing `turn_seq`. The token is the proof — every `mark_done` / `mark_failed` carries it, and the impl gates writes on `WHERE turn_seq = $token`. If the worker died and its zombie returns after another worker bumped the seq, its writes match nothing and silently no-op. In-mem this is a `Mutex<HashMap>` check; Pg it's a `WHERE` clause. Same semantics.

On claim, the impl also resets any orphan rows (status `processing`, `turn_seq < new_turn_seq`) back to `pending` and increments their `attempts`. After `attempts >= 3` the row is marked `failed` with `reason = poison`, so a prompt that reliably crashes the worker can't pin a session forever.

### Response delivery

In-mem only for now. Worker grabs (or creates) a `broadcast::Sender<ResponseChunk>` for the request, sends each chunk as it arrives, terminates the stream with a `Done` or `Error` chunk. SSE handler subscribes, forwards. On reconnect with `Last-Event-ID`, the handler reads any persisted earlier chunks from a `Vec<ResponseChunk>` kept on the request, then attaches to the live stream from there. Buffer is capped per request; if a slow client falls behind the cap, it gets a `Stalled` event and must reconnect with `Last-Event-ID` to catch up via the persisted log.

When Postgres lands, the same trait gets a `PgResponseSink` that writes to `prompt_response_chunks` and `pg_notify`s a hint (request_id + seq). One shared LISTEN connection at the process level fans out to per-stream `tokio::sync::broadcast` receivers, plus a 1–2s safety-net re-read of the chunks table to recover any dropped NOTIFY. The handler code on the SSE side doesn't change.

### Caps (named constants in `*/limits.rs`)

- `MAX_WORKERS` — bounded `JoinSet` size.
- `MAX_PENDING_PER_SESSION` — reject `enqueue` when exceeded; protects against client storms.
- `MAX_TURN_DURATION` — `tokio::time::timeout` on `run_turn`.
- `LEASE_TTL` and heartbeat at `LEASE_TTL / 3`.
- `MAX_ATTEMPTS = 3` — poison cap.
- `MAX_CHUNK_BUFFER_PER_REQUEST` — broadcast channel size.
- `MAX_PROMPT_BYTES`, `MAX_RESPONSE_BYTES` — boundary length checks.

Per CLAUDE.md §5, none of these are magic numbers — all named, documented with *why this number*, and exported.

### What changes when Postgres lands

- New crates: `sqlx`, migrations.
- `PgPromptQueue`, `PgLeaseManager`, `PgResponseSink/Source` — drop-in for the in-mem ones. Same trait, same tests (with Pg-specific contention tests added on top via `#[sqlx::test]`).
- The single binary grows two subcommands: `serve-http` and `serve-worker`. Same code; deploy decides which to run. In dev, `serve-all` runs both in-process exactly like today.
- RLS gets added to every table. Worker connection sets `app.user_id` per leased turn via `SET LOCAL` inside a transaction so pool reuse can't leak.
- SSE handler adds the shared LISTEN connection and the safety-net timer.
- `idempotency_key` becomes `UNIQUE (user_id, idempotency_key)` so different users can use the same key without collision.

Nothing in the agent loop, hook system, session store, worker pool, or SSE handler changes. That's the test for whether the trait split was right — and if any of it does need changing, that's the signal we got the trait shape wrong and should fix it before merging Postgres.

### Build order

1. `PromptQueue` + `LeaseManager` traits + `InMemory*` impls. Tests for claim-and-drain, lease expiry on `TestClock`, fencing rejects stale writes, attempts cap, queue cap.
2. Worker pool with bounded `JoinSet`, heartbeat, `run_turn` integration with the existing agent core. Test orphan recovery via `task::abort` mid-turn.
3. `ResponseSink`/`ResponseSource` + in-mem broadcast. Test publish/subscribe + replay-on-late-subscribe + cap behavior.
4. HTTP surface with axum: `POST /sessions`, `POST /prompts`, `GET /requests/:id/stream`, `POST /requests/:id/cancel`. Tower `TraceLayer` for the root span per CLAUDE.md §2.
5. Cancellation end-to-end test: post prompt, cancel, verify the in-flight turn finishes and the next is skipped.
6. *(Later)* Postgres impls + migrations + RLS + LISTEN/NOTIFY for SSE + binary split.

That's the whole thing. Boring on purpose.
