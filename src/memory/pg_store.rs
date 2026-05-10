//! Postgres-backed [`MemoryStore`] (doc/memory.md §2.1).
//!
//! All wall-clock values come from the injected [`SharedClock`] (CLAUDE.md
//! §11). Status enums and ids cross the SQL boundary via the `sqlx::Type`
//! impls in [`super::types`]; no hand-rolled string matching survives here.

use std::fmt;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::{PgPool, Row};

use crate::agents::AgentId;
use crate::clock::SharedClock;
use crate::runtime::PromptRequestId;

use super::limits::MAX_MEMORIES_PER_AGENT;
use super::store::{
    MemoryEvent, MemoryMutation, MemoryRow, MemoryStore, MemoryStoreError, MutationOutcome,
    MutationSource,
};
use super::types::{
    MemoryContent, MemoryEventId, MemoryId, MemoryKind, MemoryState, MutationKind,
    MutationSourceKind,
};

/// Column list reused by every `agent_memories` SELECT — keeping it in one
/// place removes drift between `list` / `get` / `lock_existing` and the
/// `RETURNING` clauses on the mutation paths.
const MEMORY_ROW_COLUMNS: &str = "id, agent_id, kind, content, state, pinned, source_turn_id, \
                                  created_at, last_validated_at, last_accessed_at, access_count";

const MEMORY_EVENT_COLUMNS: &str = "id, agent_id, mutation, target_memory_id, content_before, content_after, \
     source_kind, source_turn_id, created_at";

/// Postgres-backed memory store.
pub struct PgMemoryStore {
    pool: PgPool,
    clock: SharedClock,
}

impl PgMemoryStore {
    #[must_use]
    pub fn new(pool: PgPool, clock: SharedClock) -> Self {
        Self { pool, clock }
    }

    fn now(&self) -> DateTime<Utc> {
        DateTime::<Utc>::from(self.clock.now_wall())
    }
}

impl fmt::Debug for PgMemoryStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PgMemoryStore").finish_non_exhaustive()
    }
}

#[async_trait]
impl MemoryStore for PgMemoryStore {
    async fn apply(&self, mutation: MemoryMutation) -> Result<MutationOutcome, MemoryStoreError> {
        let now = self.now();
        let mut tx = self.pool.begin().await?;

        let outcome = match mutation {
            MemoryMutation::Write {
                agent,
                kind,
                content,
                state,
                pinned,
                source,
            } => {
                apply_write(
                    &mut tx,
                    WriteArgs {
                        agent,
                        kind,
                        content,
                        state,
                        pinned,
                        source,
                    },
                    now,
                )
                .await?
            }
            MemoryMutation::Update {
                agent,
                target,
                content,
                state,
                source,
                operator_override,
            } => {
                apply_update(
                    &mut tx,
                    UpdateArgs {
                        agent,
                        target,
                        content,
                        state,
                        source,
                        operator_override,
                    },
                    now,
                )
                .await?
            }
            MemoryMutation::Forget {
                agent,
                target,
                source,
                operator_override,
            } => {
                apply_forget(
                    &mut tx,
                    ForgetArgs {
                        agent,
                        target,
                        source,
                        operator_override,
                    },
                    now,
                )
                .await?
            }
        };

        tx.commit().await?;
        Ok(outcome)
    }

    async fn list(&self, agent: AgentId) -> Result<Vec<MemoryRow>, MemoryStoreError> {
        // §5: every batch capped. The cap exceeds [`MAX_MEMORIES_PER_AGENT`]
        // by one so the assertion below catches a quota overshoot — that
        // would mean the writer ignored the same cap.
        let probe_limit = i64::try_from(MAX_MEMORIES_PER_AGENT)
            .expect("invariant: MAX_MEMORIES_PER_AGENT fits in i64")
            + 1;
        let sql = format!(
            "SELECT {MEMORY_ROW_COLUMNS} FROM agent_memories
             WHERE agent_id = $1
             ORDER BY created_at ASC, id ASC
             LIMIT $2",
        );
        let rows = sqlx::query(&sql)
            .bind(agent)
            .bind(probe_limit)
            .fetch_all(&self.pool)
            .await?;

        assert!(
            rows.len() <= MAX_MEMORIES_PER_AGENT,
            "invariant: per-agent memory cap {MAX_MEMORIES_PER_AGENT} overshot ({})",
            rows.len()
        );

        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            out.push(decode_memory_row(&row)?);
        }
        Ok(out)
    }

    async fn get(&self, id: MemoryId) -> Result<Option<MemoryRow>, MemoryStoreError> {
        let sql = format!("SELECT {MEMORY_ROW_COLUMNS} FROM agent_memories WHERE id = $1");
        let row = sqlx::query(&sql)
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;

        match row {
            Some(r) => Ok(Some(decode_memory_row(&r)?)),
            None => Ok(None),
        }
    }

    async fn list_events(&self, agent: AgentId) -> Result<Vec<MemoryEvent>, MemoryStoreError> {
        // §5: events have no per-agent quota of their own (the journal
        // grows with every mutation), but we still cap the page so a long
        // history cannot OOM the worker. Callers that need full history
        // walk the journal externally; for in-process audit we stop at
        // [`MAX_EVENTS_PER_PAGE`].
        let limit = i64::try_from(super::limits::MAX_EVENTS_PER_PAGE)
            .expect("invariant: MAX_EVENTS_PER_PAGE fits in i64");
        let sql = format!(
            "SELECT {MEMORY_EVENT_COLUMNS} FROM memory_events
             WHERE agent_id = $1
             ORDER BY created_at ASC, id ASC
             LIMIT $2",
        );
        let rows = sqlx::query(&sql)
            .bind(agent)
            .bind(limit)
            .fetch_all(&self.pool)
            .await?;

        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            out.push(decode_memory_event(&row)?);
        }
        Ok(out)
    }

    async fn rebuild_materialized(&self, agent: AgentId) -> Result<(), MemoryStoreError> {
        let mut tx = self.pool.begin().await?;

        sqlx::query("DELETE FROM agent_memories WHERE agent_id = $1")
            .bind(agent)
            .execute(&mut *tx)
            .await?;

        let sql = format!(
            "SELECT {MEMORY_EVENT_COLUMNS} FROM memory_events
             WHERE agent_id = $1
             ORDER BY created_at ASC, id ASC",
        );
        let event_rows = sqlx::query(&sql).bind(agent).fetch_all(&mut *tx).await?;

        for row in event_rows {
            let event = decode_memory_event(&row)?;
            apply_replay(&mut tx, agent, &event).await?;
        }

        tx.commit().await?;
        Ok(())
    }
}

/// Inputs to [`apply_write`]. Mirrors [`MemoryMutation::Write`]; the
/// dispatcher destructures the variant once so helper signatures stay
/// short and the variant cannot be smuggled past its arm.
struct WriteArgs {
    agent: AgentId,
    kind: MemoryKind,
    content: MemoryContent,
    state: MemoryState,
    pinned: bool,
    source: MutationSource,
}

struct UpdateArgs {
    agent: AgentId,
    target: MemoryId,
    content: MemoryContent,
    state: MemoryState,
    source: MutationSource,
    operator_override: bool,
}

struct ForgetArgs {
    agent: AgentId,
    target: MemoryId,
    source: MutationSource,
    operator_override: bool,
}

async fn apply_write(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    args: WriteArgs,
    now: DateTime<Utc>,
) -> Result<MutationOutcome, MemoryStoreError> {
    let memory_id = MemoryId::new();
    let event_id = MemoryEventId::new();
    let source_turn = args.source.turn_id();

    insert_event(
        tx,
        EventInsert {
            event_id,
            agent: args.agent,
            mutation: MutationKind::Write,
            target: memory_id,
            content_before: None,
            content_after: Some(args.content.as_str()),
            source: args.source,
            now,
        },
    )
    .await?;

    let sql = format!(
        "INSERT INTO agent_memories
             (id, agent_id, kind, content, state, pinned,
              source_turn_id,
              created_at, last_validated_at, last_accessed_at, access_count)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $8, $8, 0)
         RETURNING {MEMORY_ROW_COLUMNS}",
    );
    let row = sqlx::query(&sql)
        .bind(memory_id)
        .bind(args.agent)
        .bind(args.kind)
        .bind(args.content.as_str())
        .bind(args.state)
        .bind(args.pinned)
        .bind(source_turn)
        .bind(now)
        .fetch_one(&mut **tx)
        .await?;

    Ok(MutationOutcome {
        event_id,
        memory_id,
        row: Some(decode_memory_row(&row)?),
    })
}

async fn apply_update(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    args: UpdateArgs,
    now: DateTime<Utc>,
) -> Result<MutationOutcome, MemoryStoreError> {
    let prior = lock_existing(tx, args.agent, args.target, args.operator_override).await?;
    let event_id = MemoryEventId::new();

    insert_event(
        tx,
        EventInsert {
            event_id,
            agent: args.agent,
            mutation: MutationKind::Update,
            target: args.target,
            content_before: Some(prior.content.as_str()),
            content_after: Some(args.content.as_str()),
            source: args.source,
            now,
        },
    )
    .await?;

    let sql = format!(
        "UPDATE agent_memories SET content = $1, state = $2 WHERE id = $3
         RETURNING {MEMORY_ROW_COLUMNS}",
    );
    let row = sqlx::query(&sql)
        .bind(args.content.as_str())
        .bind(args.state)
        .bind(args.target)
        .fetch_one(&mut **tx)
        .await?;

    Ok(MutationOutcome {
        event_id,
        memory_id: args.target,
        row: Some(decode_memory_row(&row)?),
    })
}

async fn apply_forget(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    args: ForgetArgs,
    now: DateTime<Utc>,
) -> Result<MutationOutcome, MemoryStoreError> {
    let prior = lock_existing(tx, args.agent, args.target, args.operator_override).await?;
    let event_id = MemoryEventId::new();

    insert_event(
        tx,
        EventInsert {
            event_id,
            agent: args.agent,
            mutation: MutationKind::Forget,
            target: args.target,
            content_before: Some(prior.content.as_str()),
            content_after: None,
            source: args.source,
            now,
        },
    )
    .await?;

    sqlx::query("DELETE FROM agent_memories WHERE id = $1")
        .bind(args.target)
        .execute(&mut **tx)
        .await?;

    Ok(MutationOutcome {
        event_id,
        memory_id: args.target,
        row: None,
    })
}

/// Single binding shape for every journal append. The column constraint
/// (`memory_events_content_shape`) enforces the per-mutation invariant on
/// `content_before` / `content_after`; this helper just funnels the binds
/// through one query string.
struct EventInsert<'a> {
    event_id: MemoryEventId,
    agent: AgentId,
    mutation: MutationKind,
    target: MemoryId,
    content_before: Option<&'a str>,
    content_after: Option<&'a str>,
    source: MutationSource,
    now: DateTime<Utc>,
}

async fn insert_event(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    e: EventInsert<'_>,
) -> Result<(), MemoryStoreError> {
    sqlx::query(
        "INSERT INTO memory_events
             (id, agent_id, mutation, target_memory_id,
              content_before, content_after,
              source_kind, source_turn_id, created_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(e.event_id)
    .bind(e.agent)
    .bind(e.mutation)
    .bind(e.target)
    .bind(e.content_before)
    .bind(e.content_after)
    .bind(e.source.kind())
    .bind(e.source.turn_id())
    .bind(e.now)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Lock the materialized row for an update / forget; verify ownership and
/// pinned-immunity. Returns the row snapshot so callers can copy
/// `content_before` onto the journal event without a second read.
async fn lock_existing(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    agent: AgentId,
    target: MemoryId,
    operator_override: bool,
) -> Result<MemoryRow, MemoryStoreError> {
    let sql = format!("SELECT {MEMORY_ROW_COLUMNS} FROM agent_memories WHERE id = $1 FOR UPDATE",);
    let row = sqlx::query(&sql)
        .bind(target)
        .fetch_optional(&mut **tx)
        .await?;

    let row = row.ok_or(MemoryStoreError::NotFound { id: target })?;
    let parsed = decode_memory_row(&row)?;

    if parsed.agent_id != agent {
        return Err(MemoryStoreError::WrongAgent { id: target, agent });
    }
    if parsed.pinned && !operator_override {
        return Err(MemoryStoreError::PinnedImmutable { id: target });
    }

    Ok(parsed)
}

/// Apply one journal event to the materialized table during a rebuild.
/// Mirrors the live mutation paths but skips the journal append (we are
/// reading from it) and the pinned-immunity check (replay must be
/// faithful — if the original mutation was allowed, the rebuild is too).
///
/// Phase 1 only journals content; the kind / state / pinned fields are
/// reconstructed from defaults. Phase 5 will extend the event payload so
/// the rebuild reproduces them faithfully.
async fn apply_replay(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    agent: AgentId,
    event: &MemoryEvent,
) -> Result<(), MemoryStoreError> {
    match event.mutation {
        MutationKind::Write => {
            // §6: column CHECK on `memory_events_content_shape` makes a
            // write event without `content_after` schema-corrupt.
            let content = event
                .content_after
                .as_ref()
                .expect("invariant: write event must carry content_after (CHECK constraint)");
            sqlx::query(
                "INSERT INTO agent_memories
                     (id, agent_id, kind, content, state, pinned,
                      source_turn_id,
                      created_at, last_validated_at, last_accessed_at, access_count)
                 VALUES ($1, $2, $3, $4, $5, FALSE, $6, $7, $7, $7, 0)",
            )
            .bind(event.target_memory_id)
            .bind(agent)
            .bind(MemoryKind::Identity)
            .bind(content.as_str())
            .bind(MemoryState::Tentative)
            .bind(event.source.turn_id())
            .bind(event.created_at)
            .execute(&mut **tx)
            .await?;
        }
        MutationKind::Update => {
            let content = event
                .content_after
                .as_ref()
                .expect("invariant: update event must carry content_after (CHECK constraint)");
            sqlx::query("UPDATE agent_memories SET content = $1 WHERE id = $2")
                .bind(content.as_str())
                .bind(event.target_memory_id)
                .execute(&mut **tx)
                .await?;
        }
        MutationKind::Forget => {
            sqlx::query("DELETE FROM agent_memories WHERE id = $1")
                .bind(event.target_memory_id)
                .execute(&mut **tx)
                .await?;
        }
    }
    Ok(())
}

fn decode_memory_row(row: &sqlx::postgres::PgRow) -> Result<MemoryRow, MemoryStoreError> {
    let content_raw: String = row.try_get("content")?;
    let access_count_raw: i64 = row.try_get("access_count")?;
    // §6: column CHECK guarantees non-negative; observing otherwise means
    // schema corruption.
    assert!(
        access_count_raw >= 0,
        "invariant: access_count must be non-negative, got {access_count_raw}"
    );
    let access_count =
        u64::try_from(access_count_raw).expect("invariant: non-negative i64 fits in u64");
    Ok(MemoryRow {
        id: row.try_get("id")?,
        agent_id: row.try_get("agent_id")?,
        kind: row.try_get("kind")?,
        content: MemoryContent::try_from(content_raw)?,
        state: row.try_get("state")?,
        pinned: row.try_get("pinned")?,
        source_turn_id: row.try_get::<Option<PromptRequestId>, _>("source_turn_id")?,
        created_at: row.try_get("created_at")?,
        last_validated_at: row.try_get("last_validated_at")?,
        last_accessed_at: row.try_get("last_accessed_at")?,
        access_count,
    })
}

fn decode_memory_event(row: &sqlx::postgres::PgRow) -> Result<MemoryEvent, MemoryStoreError> {
    let content_before_raw: Option<String> = row.try_get("content_before")?;
    let content_after_raw: Option<String> = row.try_get("content_after")?;
    let source_kind: MutationSourceKind = row.try_get("source_kind")?;
    let source_turn_id: Option<PromptRequestId> = row.try_get("source_turn_id")?;

    // §6: `memory_events_source_turn` CHECK enforces this pairing.
    let source = match source_kind {
        MutationSourceKind::Turn => {
            let id =
                source_turn_id.expect("invariant: source_kind = 'turn' must carry source_turn_id");
            MutationSource::Turn(id)
        }
        MutationSourceKind::Operator => MutationSource::Operator,
        MutationSourceKind::Librarian => MutationSource::Librarian,
    };

    let content_before = content_before_raw
        .map(MemoryContent::try_from)
        .transpose()?;
    let content_after = content_after_raw.map(MemoryContent::try_from).transpose()?;

    Ok(MemoryEvent {
        id: row.try_get("id")?,
        agent_id: row.try_get("agent_id")?,
        mutation: row.try_get("mutation")?,
        target_memory_id: row.try_get("target_memory_id")?,
        content_before,
        content_after,
        source,
        created_at: row.try_get("created_at")?,
    })
}
