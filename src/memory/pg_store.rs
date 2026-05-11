//! Postgres-backed [`MemoryStore`] (doc/memory.md §2.1, §2.5–§2.9).
//!
//! All wall-clock values come from the injected [`SharedClock`] (CLAUDE.md
//! §11). Status enums and ids cross the SQL boundary via the `sqlx::Type`
//! impls in [`super::types`]; no hand-rolled string matching survives here.
//!
//! An optional [`SharedEmbeddingProvider`] drives vector storage — when
//! present, every `Write` / `Update` synchronously embeds the new
//! content and stores the vector on `agent_memories.embedding`. When
//! absent, writes leave `embedding` NULL and retrieval returns empty
//! results gracefully.

use std::fmt;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::{PgPool, Row};

use crate::agents::AgentId;
use crate::clock::SharedClock;
use crate::provider::SharedEmbeddingProvider;
use crate::runtime::PromptRequestId;

use super::limits::{MAX_EVENTS_PER_PAGE, MAX_MEMORIES_PER_AGENT, MAX_SIMILAR_PAIRS_PER_AGENT};
use super::store::{
    ContradictionEventRow, MemoryEvent, MemoryMutation, MemoryRow, MemoryStore, MemoryStoreError,
    MutationOutcome, MutationSource, PairCandidate, ResolutionOutcome, ScoredMemoryRow,
    SearchFilter, ValidationSource,
};
use super::types::{
    ContradictionEventId, MemoryContent, MemoryEventId, MemoryId, MemoryKind, MemoryState,
    MutationKind, MutationSourceKind,
};
use super::vector;

/// Column list reused by every `agent_memories` SELECT — keeping it in one
/// place removes drift between `list` / `get` / `lock_existing` and the
/// `RETURNING` clauses on the mutation paths.
const MEMORY_ROW_COLUMNS: &str = "id, agent_id, kind, content, state, pinned, source_turn_id, \
                                  created_at, last_validated_at, last_accessed_at, access_count";

const MEMORY_EVENT_COLUMNS: &str = "id, agent_id, mutation, target_memory_id, content_before, content_after, \
     source_kind, source_turn_id, created_at, kind, state, pinned";

/// Postgres-backed memory store.
///
/// The embedding provider is non-optional: every write/update embeds
/// synchronously, every retrieval has a vector to match against. The
/// memory subsystem refuses to start without one (`Settings::embedding`
/// is required); see doc/memory.md §2.9.
pub struct PgMemoryStore {
    pool: PgPool,
    clock: SharedClock,
    embeddings: SharedEmbeddingProvider,
}

impl PgMemoryStore {
    #[must_use]
    pub fn new(pool: PgPool, clock: SharedClock, embeddings: SharedEmbeddingProvider) -> Self {
        Self {
            pool,
            clock,
            embeddings,
        }
    }

    fn now(&self) -> DateTime<Utc> {
        DateTime::<Utc>::from(self.clock.now_wall())
    }

    /// Embed `content` synchronously. Errors propagate so the mutation
    /// aborts before journaling — no orphaned rows without embeddings.
    async fn embed(&self, content: &str) -> Result<Vec<f32>, MemoryStoreError> {
        crate::provider::embed_one(self.embeddings.as_ref(), content)
            .await
            .map_err(MemoryStoreError::Provider)
    }
}

impl fmt::Debug for PgMemoryStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PgMemoryStore").finish_non_exhaustive()
    }
}

#[async_trait]
#[allow(clippy::too_many_lines)] // dispatch on three variants + helpers
impl MemoryStore for PgMemoryStore {
    async fn apply(&self, mutation: MemoryMutation) -> Result<MutationOutcome, MemoryStoreError> {
        // Embed BEFORE opening the transaction — embedding can be slow,
        // and we don't want to hold locks across a network call. `Forget`
        // touches no content so it skips the call.
        let embedding: Option<Vec<f32>> = match &mutation {
            MemoryMutation::Write { content, .. } | MemoryMutation::Update { content, .. } => {
                Some(self.embed(content.as_str()).await?)
            }
            MemoryMutation::Forget { .. } => None,
        };

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
                        embedding,
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
                        embedding,
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
        let limit =
            i64::try_from(MAX_EVENTS_PER_PAGE).expect("invariant: MAX_EVENTS_PER_PAGE fits in i64");
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

    async fn search_by_embedding(
        &self,
        agent: AgentId,
        embedding: &[f32],
        k: usize,
        filter: SearchFilter,
    ) -> Result<Vec<ScoredMemoryRow>, MemoryStoreError> {
        if embedding.is_empty() || k == 0 {
            return Ok(Vec::new());
        }
        let limit = i64::try_from(k.min(MAX_MEMORIES_PER_AGENT))
            .expect("invariant: k ≤ MAX_MEMORIES_PER_AGENT");

        let kinds_filter: Vec<String> = filter.kinds.as_ref().map_or_else(Vec::new, |ks| {
            ks.iter().map(|k| k.as_str().to_owned()).collect()
        });
        let min_state_priority: Option<i32> = filter.min_state.map(|s| i32::from(s.priority()));

        let sql = format!(
            "SELECT {MEMORY_ROW_COLUMNS},
                    1 - (embedding <=> $2::vector) AS similarity
             FROM agent_memories
             WHERE agent_id = $1
               AND embedding IS NOT NULL
               AND ($3::text[] IS NULL OR kind = ANY($3))
               AND ($4::int IS NULL OR
                    CASE state
                      WHEN 'core'      THEN 4
                      WHEN 'validated' THEN 3
                      WHEN 'held'      THEN 2
                      WHEN 'tentative' THEN 1
                    END >= $4)
             ORDER BY embedding <=> $2::vector ASC
             LIMIT $5",
        );

        let kinds_arg = if kinds_filter.is_empty() {
            None
        } else {
            Some(kinds_filter)
        };

        let rows = sqlx::query(&sql)
            .bind(agent)
            .bind(vector::encode(embedding))
            .bind(kinds_arg)
            .bind(min_state_priority)
            .bind(limit)
            .fetch_all(&self.pool)
            .await?;

        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let similarity: f64 = row.try_get("similarity")?;
            let similarity_f32 = f64_to_f32_clamped(similarity);
            out.push(ScoredMemoryRow {
                row: decode_memory_row(&row)?,
                similarity: similarity_f32,
            });
        }
        Ok(out)
    }

    async fn similar_pairs(
        &self,
        agent: AgentId,
        threshold: f32,
        max_pairs: usize,
    ) -> Result<Vec<PairCandidate>, MemoryStoreError> {
        let limit = i64::try_from(max_pairs.min(MAX_SIMILAR_PAIRS_PER_AGENT))
            .expect("invariant: max_pairs fits in i64");
        let f64_threshold = f64::from(threshold);

        let sql = format!(
            "WITH base AS (
                 SELECT {MEMORY_ROW_COLUMNS}, embedding FROM agent_memories
                 WHERE agent_id = $1 AND embedding IS NOT NULL
             )
             SELECT a.id AS a_id, a.agent_id AS a_agent_id, a.kind AS a_kind, a.content AS a_content,
                    a.state AS a_state, a.pinned AS a_pinned, a.source_turn_id AS a_source_turn_id,
                    a.created_at AS a_created_at, a.last_validated_at AS a_last_validated_at,
                    a.last_accessed_at AS a_last_accessed_at, a.access_count AS a_access_count,
                    b.id AS b_id, b.agent_id AS b_agent_id, b.kind AS b_kind, b.content AS b_content,
                    b.state AS b_state, b.pinned AS b_pinned, b.source_turn_id AS b_source_turn_id,
                    b.created_at AS b_created_at, b.last_validated_at AS b_last_validated_at,
                    b.last_accessed_at AS b_last_accessed_at, b.access_count AS b_access_count,
                    1 - (a.embedding <=> b.embedding) AS similarity
             FROM base a
             JOIN base b ON a.id < b.id
             WHERE 1 - (a.embedding <=> b.embedding) >= $2
             ORDER BY similarity DESC
             LIMIT $3",
        );

        let rows = sqlx::query(&sql)
            .bind(agent)
            .bind(f64_threshold)
            .bind(limit)
            .fetch_all(&self.pool)
            .await?;

        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let similarity: f64 = row.try_get("similarity")?;
            let similarity_f32 = f64_to_f32_clamped(similarity);
            out.push(PairCandidate {
                a: decode_pair_side(&row, &PAIR_COLS_A)?,
                b: decode_pair_side(&row, &PAIR_COLS_B)?,
                similarity: similarity_f32,
            });
        }
        Ok(out)
    }

    async fn decay_validated(
        &self,
        agent: AgentId,
        cutoff: DateTime<Utc>,
    ) -> Result<usize, MemoryStoreError> {
        let now = self.now();
        let mut tx = self.pool.begin().await?;

        // Find candidates: validated, non-pinned, last_validated_at < cutoff.
        let sql = format!(
            "SELECT {MEMORY_ROW_COLUMNS} FROM agent_memories
             WHERE agent_id = $1 AND state = 'validated' AND pinned = FALSE
               AND last_validated_at < $2
             FOR UPDATE",
        );
        let rows = sqlx::query(&sql)
            .bind(agent)
            .bind(cutoff)
            .fetch_all(&mut *tx)
            .await?;

        let mut demoted = 0usize;
        for row in rows {
            let parsed = decode_memory_row(&row)?;
            // Append journal event recording the state demotion.
            let event_id = MemoryEventId::new();
            insert_event(
                &mut tx,
                EventInsert {
                    event_id,
                    agent: parsed.agent_id,
                    mutation: MutationKind::Update,
                    target: parsed.id,
                    content_before: Some(parsed.content.as_str()),
                    content_after: Some(parsed.content.as_str()),
                    source: MutationSource::Librarian,
                    now,
                    kind: Some(parsed.kind),
                    state: Some(MemoryState::Held),
                    pinned: Some(parsed.pinned),
                },
            )
            .await?;
            sqlx::query("UPDATE agent_memories SET state = 'held' WHERE id = $1")
                .bind(parsed.id)
                .execute(&mut *tx)
                .await?;
            demoted += 1;
        }

        tx.commit().await?;
        Ok(demoted)
    }

    async fn record_validation(
        &self,
        agent: AgentId,
        memory: MemoryId,
        source: ValidationSource,
        detail: Option<&str>,
    ) -> Result<MemoryRow, MemoryStoreError> {
        let now = self.now();
        let mut tx = self.pool.begin().await?;

        let prior = lock_existing(&mut tx, agent, memory, true).await?;

        sqlx::query(
            "INSERT INTO validation_events
                 (id, agent_id, memory_id, source, detail, created_at)
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(uuid::Uuid::new_v4())
        .bind(agent)
        .bind(memory)
        .bind(source.as_str())
        .bind(detail)
        .bind(now)
        .execute(&mut *tx)
        .await?;

        // Promotion rule: Tentative → Held; Held → Validated. Pinned and
        // Core stay put (Core is the operator-pinned floor; pinned rows
        // are exempt from agent-driven transitions, but operator-driven
        // validation can still bump non-Core states).
        let new_state = if prior.pinned && prior.state == MemoryState::Core {
            MemoryState::Core
        } else {
            match prior.state {
                MemoryState::Tentative => MemoryState::Held,
                MemoryState::Held => MemoryState::Validated,
                MemoryState::Validated | MemoryState::Core => prior.state,
            }
        };

        let event_id = MemoryEventId::new();
        insert_event(
            &mut tx,
            EventInsert {
                event_id,
                agent,
                mutation: MutationKind::Update,
                target: memory,
                content_before: Some(prior.content.as_str()),
                content_after: Some(prior.content.as_str()),
                source: MutationSource::Librarian,
                now,
                kind: Some(prior.kind),
                state: Some(new_state),
                pinned: Some(prior.pinned),
            },
        )
        .await?;

        let sql = format!(
            "UPDATE agent_memories
             SET state = $1, last_validated_at = $2
             WHERE id = $3
             RETURNING {MEMORY_ROW_COLUMNS}",
        );
        let row = sqlx::query(&sql)
            .bind(new_state)
            .bind(now)
            .bind(memory)
            .fetch_one(&mut *tx)
            .await?;

        tx.commit().await?;
        decode_memory_row(&row)
    }

    async fn record_contradiction(
        &self,
        agent: AgentId,
        a: MemoryId,
        b: MemoryId,
        reason: &str,
    ) -> Result<ContradictionEventId, MemoryStoreError> {
        // Canonicalise the pair so duplicate detection works regardless of
        // call order.
        let (lo, hi) = if a.as_uuid() < b.as_uuid() {
            (a, b)
        } else {
            (b, a)
        };
        let now = self.now();

        // Idempotency: if an unresolved row already exists for this pair,
        // return its id.
        let existing: Option<(uuid::Uuid,)> = sqlx::query_as(
            "SELECT id FROM contradiction_events
             WHERE agent_id = $1 AND memory_a = $2 AND memory_b = $3 AND resolved_at IS NULL
             ORDER BY created_at DESC LIMIT 1",
        )
        .bind(agent)
        .bind(lo)
        .bind(hi)
        .fetch_optional(&self.pool)
        .await?;
        if let Some((id,)) = existing {
            return Ok(ContradictionEventId::from(id));
        }

        let id = ContradictionEventId::new();
        sqlx::query(
            "INSERT INTO contradiction_events
                 (id, agent_id, memory_a, memory_b, reason, created_at)
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(id)
        .bind(agent)
        .bind(lo)
        .bind(hi)
        .bind(reason)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(id)
    }

    async fn unresolved_contradictions(
        &self,
        agent: AgentId,
    ) -> Result<Vec<ContradictionEventRow>, MemoryStoreError> {
        let limit = i64::try_from(MAX_SIMILAR_PAIRS_PER_AGENT).expect("invariant: cap fits in i64");
        let rows = sqlx::query(
            "SELECT id, agent_id, memory_a, memory_b, reason, created_at,
                    resolved_at, resolution_event_id, resolution_reason
             FROM contradiction_events
             WHERE agent_id = $1 AND resolved_at IS NULL
             ORDER BY created_at ASC
             LIMIT $2",
        )
        .bind(agent)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;

        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            out.push(decode_contradiction_row(&row)?);
        }
        Ok(out)
    }

    async fn read_contradiction(
        &self,
        id: ContradictionEventId,
    ) -> Result<Option<ContradictionEventRow>, MemoryStoreError> {
        let row = sqlx::query(
            "SELECT id, agent_id, memory_a, memory_b, reason, created_at,
                    resolved_at, resolution_event_id, resolution_reason
             FROM contradiction_events WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        match row {
            Some(r) => Ok(Some(decode_contradiction_row(&r)?)),
            None => Ok(None),
        }
    }

    async fn resolve_contradiction(
        &self,
        id: ContradictionEventId,
        outcome: ResolutionOutcome,
    ) -> Result<(), MemoryStoreError> {
        let now = self.now();
        let (event_id, reason): (Option<MemoryEventId>, Option<String>) = match outcome {
            ResolutionOutcome::Mutation(event_id) => (Some(event_id), None),
            ResolutionOutcome::NoAction { reason } => (None, Some(reason.into_inner())),
        };
        sqlx::query(
            "UPDATE contradiction_events
             SET resolved_at = $1, resolution_event_id = $2, resolution_reason = $3
             WHERE id = $4 AND resolved_at IS NULL",
        )
        .bind(now)
        .bind(event_id)
        .bind(reason)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn evict_overflow(
        &self,
        agent: AgentId,
        quota: usize,
    ) -> Result<Vec<MemoryId>, MemoryStoreError> {
        let now = self.now();
        let mut tx = self.pool.begin().await?;

        // Score: state_priority (4..1) * 1e6 + log access_count + recency.
        // Pinned rows are excluded; we evict the bottom of the non-pinned
        // bucket until the per-agent count is at most `quota`.
        let count: (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM agent_memories WHERE agent_id = $1")
                .bind(agent)
                .fetch_one(&mut *tx)
                .await?;
        let total = usize::try_from(count.0).expect("invariant: COUNT non-negative");
        if total <= quota {
            tx.commit().await?;
            return Ok(Vec::new());
        }
        let to_evict_target = total - quota;
        let evict_limit = i64::try_from(to_evict_target).expect("invariant: target fits in i64");

        let sql = format!(
            "SELECT {MEMORY_ROW_COLUMNS} FROM agent_memories
             WHERE agent_id = $1 AND pinned = FALSE
             ORDER BY
               CASE state
                 WHEN 'core'      THEN 4
                 WHEN 'validated' THEN 3
                 WHEN 'held'      THEN 2
                 WHEN 'tentative' THEN 1
               END ASC,
               last_accessed_at ASC,
               access_count ASC
             LIMIT $2",
        );
        let rows = sqlx::query(&sql)
            .bind(agent)
            .bind(evict_limit)
            .fetch_all(&mut *tx)
            .await?;

        let mut evicted = Vec::with_capacity(rows.len());
        for row in rows {
            let parsed = decode_memory_row(&row)?;
            let event_id = MemoryEventId::new();
            insert_event(
                &mut tx,
                EventInsert {
                    event_id,
                    agent,
                    mutation: MutationKind::Forget,
                    target: parsed.id,
                    content_before: Some(parsed.content.as_str()),
                    content_after: None,
                    source: MutationSource::Librarian,
                    now,
                    kind: None,
                    state: None,
                    pinned: None,
                },
            )
            .await?;
            sqlx::query("DELETE FROM agent_memories WHERE id = $1")
                .bind(parsed.id)
                .execute(&mut *tx)
                .await?;
            evicted.push(parsed.id);
        }

        tx.commit().await?;
        Ok(evicted)
    }

    async fn revert_event(
        &self,
        agent: AgentId,
        event: MemoryEventId,
    ) -> Result<Option<MemoryRow>, MemoryStoreError> {
        let now = self.now();
        let mut tx = self.pool.begin().await?;

        // Read the event to determine the inverse operation.
        let sql = format!(
            "SELECT {MEMORY_EVENT_COLUMNS} FROM memory_events WHERE id = $1 AND agent_id = $2",
        );
        let row = sqlx::query(&sql)
            .bind(event)
            .bind(agent)
            .fetch_optional(&mut *tx)
            .await?;
        let evt = row
            .ok_or_else(|| MemoryStoreError::NotFound {
                id: MemoryId::new(),
            })
            .map(|r| decode_memory_event(&r))??;

        let target = evt.target_memory_id;
        // Inverse:
        //   write   -> forget
        //   update  -> update back to content_before
        //   forget  -> write back content_before with original kind/state/pinned
        //              (we stored those on the event)
        let inverse_event_id = MemoryEventId::new();
        match evt.mutation {
            MutationKind::Write => {
                let prior = lock_existing(&mut tx, agent, target, true).await.ok();
                let content_before = prior
                    .as_ref()
                    .map(|p| p.content.as_str())
                    .or_else(|| evt.content_after.as_ref().map(MemoryContent::as_str))
                    .unwrap_or("(unknown)");
                insert_event(
                    &mut tx,
                    EventInsert {
                        event_id: inverse_event_id,
                        agent,
                        mutation: MutationKind::Forget,
                        target,
                        content_before: Some(content_before),
                        content_after: None,
                        source: MutationSource::Operator,
                        now,
                        kind: None,
                        state: None,
                        pinned: None,
                    },
                )
                .await?;
                sqlx::query("DELETE FROM agent_memories WHERE id = $1")
                    .bind(target)
                    .execute(&mut *tx)
                    .await?;
            }
            MutationKind::Update => {
                let prior = lock_existing(&mut tx, agent, target, true).await?;
                let restore_content = evt
                    .content_before
                    .as_ref()
                    .ok_or_else(|| {
                        MemoryStoreError::Parse(crate::types::ParseError::Empty {
                            field: "content_before",
                        })
                    })?
                    .as_str();
                insert_event(
                    &mut tx,
                    EventInsert {
                        event_id: inverse_event_id,
                        agent,
                        mutation: MutationKind::Update,
                        target,
                        content_before: Some(prior.content.as_str()),
                        content_after: Some(restore_content),
                        source: MutationSource::Operator,
                        now,
                        kind: Some(prior.kind),
                        state: Some(prior.state),
                        pinned: Some(prior.pinned),
                    },
                )
                .await?;
                sqlx::query("UPDATE agent_memories SET content = $1 WHERE id = $2")
                    .bind(restore_content)
                    .bind(target)
                    .execute(&mut *tx)
                    .await?;
            }
            MutationKind::Forget => {
                // Restore using the forget event's content_before plus the
                // last journaled write/update that produced kind/state/pinned.
                let restore = restore_attrs_from_journal(&mut tx, agent, target).await?;
                let content = evt
                    .content_before
                    .as_ref()
                    .ok_or_else(|| {
                        MemoryStoreError::Parse(crate::types::ParseError::Empty {
                            field: "content_before",
                        })
                    })?
                    .as_str();
                insert_event(
                    &mut tx,
                    EventInsert {
                        event_id: inverse_event_id,
                        agent,
                        mutation: MutationKind::Write,
                        target,
                        content_before: None,
                        content_after: Some(content),
                        source: MutationSource::Operator,
                        now,
                        kind: Some(restore.kind),
                        state: Some(restore.state),
                        pinned: Some(restore.pinned),
                    },
                )
                .await?;
                sqlx::query(
                    "INSERT INTO agent_memories
                         (id, agent_id, kind, content, state, pinned,
                          source_turn_id,
                          created_at, last_validated_at, last_accessed_at, access_count)
                     VALUES ($1, $2, $3, $4, $5, $6, NULL, $7, $7, $7, 0)
                     ON CONFLICT (id) DO NOTHING",
                )
                .bind(target)
                .bind(agent)
                .bind(restore.kind)
                .bind(content)
                .bind(restore.state)
                .bind(restore.pinned)
                .bind(now)
                .execute(&mut *tx)
                .await?;
            }
        }

        let sql = format!("SELECT {MEMORY_ROW_COLUMNS} FROM agent_memories WHERE id = $1");
        let row_after = sqlx::query(&sql)
            .bind(target)
            .fetch_optional(&mut *tx)
            .await?;

        tx.commit().await?;
        match row_after {
            Some(r) => Ok(Some(decode_memory_row(&r)?)),
            None => Ok(None),
        }
    }

    async fn set_pinned(
        &self,
        agent: AgentId,
        memory: MemoryId,
        pinned: bool,
    ) -> Result<MemoryRow, MemoryStoreError> {
        let now = self.now();
        let mut tx = self.pool.begin().await?;
        let prior = lock_existing(&mut tx, agent, memory, true).await?;
        let event_id = MemoryEventId::new();
        insert_event(
            &mut tx,
            EventInsert {
                event_id,
                agent,
                mutation: MutationKind::Update,
                target: memory,
                content_before: Some(prior.content.as_str()),
                content_after: Some(prior.content.as_str()),
                source: MutationSource::Operator,
                now,
                kind: Some(prior.kind),
                state: Some(prior.state),
                pinned: Some(pinned),
            },
        )
        .await?;
        let sql = format!(
            "UPDATE agent_memories SET pinned = $1 WHERE id = $2 RETURNING {MEMORY_ROW_COLUMNS}",
        );
        let row = sqlx::query(&sql)
            .bind(pinned)
            .bind(memory)
            .fetch_one(&mut *tx)
            .await?;
        tx.commit().await?;
        decode_memory_row(&row)
    }

    async fn record_access(&self, ids: &[MemoryId]) -> Result<(), MemoryStoreError> {
        if ids.is_empty() {
            return Ok(());
        }
        assert!(
            ids.len() <= MAX_MEMORIES_PER_AGENT,
            "invariant: record_access batch ≤ MAX_MEMORIES_PER_AGENT"
        );
        let now = self.now();
        sqlx::query(
            "UPDATE agent_memories
             SET last_accessed_at = $1, access_count = access_count + 1
             WHERE id = ANY($2)",
        )
        .bind(now)
        .bind(ids)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

/// Result of [`restore_attrs_from_journal`] — the lifecycle attrs to use
/// when reverting a `forget` event back to a `write`.
struct RestoredAttrs {
    kind: MemoryKind,
    state: MemoryState,
    pinned: bool,
}

/// Walk the journal back to the most recent write/update on `target` and
/// pull the kind/state/pinned attrs from it. Used by the forget-revert
/// path; defensive defaults applied when no prior write exists.
async fn restore_attrs_from_journal(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    agent: AgentId,
    target: MemoryId,
) -> Result<RestoredAttrs, MemoryStoreError> {
    let row: Option<(Option<String>, Option<String>, Option<bool>)> = sqlx::query_as(
        "SELECT kind, state, pinned FROM memory_events
         WHERE agent_id = $1 AND target_memory_id = $2 AND mutation IN ('write','update')
         ORDER BY created_at DESC, id DESC LIMIT 1",
    )
    .bind(agent)
    .bind(target)
    .fetch_optional(&mut **tx)
    .await?;
    let row = row.unwrap_or((None, None, None));
    let kind = row
        .0
        .as_deref()
        .and_then(MemoryKind::parse)
        .unwrap_or(MemoryKind::Identity);
    let state = row
        .1
        .as_deref()
        .and_then(MemoryState::parse)
        .unwrap_or(MemoryState::Tentative);
    let pinned = row.2.unwrap_or(false);
    Ok(RestoredAttrs {
        kind,
        state,
        pinned,
    })
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
    embedding: Option<Vec<f32>>,
}

struct UpdateArgs {
    agent: AgentId,
    target: MemoryId,
    content: MemoryContent,
    state: MemoryState,
    source: MutationSource,
    operator_override: bool,
    embedding: Option<Vec<f32>>,
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
            kind: Some(args.kind),
            state: Some(args.state),
            pinned: Some(args.pinned),
        },
    )
    .await?;

    let embedding_lit: Option<String> = args.embedding.as_deref().map(vector::encode);

    let sql = format!(
        "INSERT INTO agent_memories
             (id, agent_id, kind, content, state, pinned,
              source_turn_id,
              created_at, last_validated_at, last_accessed_at, access_count, embedding)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $8, $8, 0, $9::vector)
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
        .bind(embedding_lit)
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
            kind: Some(prior.kind),
            state: Some(args.state),
            pinned: Some(prior.pinned),
        },
    )
    .await?;

    let embedding_lit: Option<String> = args.embedding.as_deref().map(vector::encode);

    let sql = format!(
        "UPDATE agent_memories SET content = $1, state = $2, embedding = $3::vector WHERE id = $4
         RETURNING {MEMORY_ROW_COLUMNS}",
    );
    let row = sqlx::query(&sql)
        .bind(args.content.as_str())
        .bind(args.state)
        .bind(embedding_lit)
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
            kind: None,
            state: None,
            pinned: None,
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
    kind: Option<MemoryKind>,
    state: Option<MemoryState>,
    pinned: Option<bool>,
}

async fn insert_event(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    e: EventInsert<'_>,
) -> Result<(), MemoryStoreError> {
    sqlx::query(
        "INSERT INTO memory_events
             (id, agent_id, mutation, target_memory_id,
              content_before, content_after,
              source_kind, source_turn_id, created_at,
              kind, state, pinned)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
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
    .bind(e.kind)
    .bind(e.state)
    .bind(e.pinned)
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
async fn apply_replay(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    agent: AgentId,
    event: &MemoryEvent,
) -> Result<(), MemoryStoreError> {
    let (kind, state, pinned) = event_replay_attrs(tx, event).await?;

    match event.mutation {
        MutationKind::Write => {
            let content = event
                .content_after
                .as_ref()
                .expect("invariant: write event must carry content_after (CHECK constraint)");
            sqlx::query(
                "INSERT INTO agent_memories
                     (id, agent_id, kind, content, state, pinned,
                      source_turn_id,
                      created_at, last_validated_at, last_accessed_at, access_count)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $8, $8, 0)",
            )
            .bind(event.target_memory_id)
            .bind(agent)
            .bind(kind.unwrap_or(MemoryKind::Identity))
            .bind(content.as_str())
            .bind(state.unwrap_or(MemoryState::Tentative))
            .bind(pinned.unwrap_or(false))
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
            sqlx::query(
                "UPDATE agent_memories SET content = $1, state = COALESCE($2, state), pinned = COALESCE($3, pinned) WHERE id = $4",
            )
            .bind(content.as_str())
            .bind(state)
            .bind(pinned)
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

/// Read the denormalized `(kind, state, pinned)` columns off a journal
/// row in one round-trip. Any of the three may be NULL on rows written
/// before the replay-attrs migration.
async fn event_replay_attrs(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    event: &MemoryEvent,
) -> Result<(Option<MemoryKind>, Option<MemoryState>, Option<bool>), MemoryStoreError> {
    let row: Option<(Option<String>, Option<String>, Option<bool>)> =
        sqlx::query_as("SELECT kind, state, pinned FROM memory_events WHERE id = $1")
            .bind(event.id)
            .fetch_optional(&mut **tx)
            .await?;
    Ok(row.map_or((None, None, None), |(k, s, p)| {
        (
            k.as_deref().and_then(MemoryKind::parse),
            s.as_deref().and_then(MemoryState::parse),
            p,
        )
    }))
}

fn decode_memory_row(row: &sqlx::postgres::PgRow) -> Result<MemoryRow, MemoryStoreError> {
    let content_raw: String = row.try_get("content")?;
    let access_count_raw: i64 = row.try_get("access_count")?;
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

/// Decode one side of a similar-pair join. Column names are prefixed with
/// `a_` or `b_` so a single SELECT returns both rows in one shot.
struct PairCols {
    id: &'static str,
    agent_id: &'static str,
    kind: &'static str,
    content: &'static str,
    state: &'static str,
    pinned: &'static str,
    source_turn_id: &'static str,
    created_at: &'static str,
    last_validated_at: &'static str,
    last_accessed_at: &'static str,
    access_count: &'static str,
}

const PAIR_COLS_A: PairCols = PairCols {
    id: "a_id",
    agent_id: "a_agent_id",
    kind: "a_kind",
    content: "a_content",
    state: "a_state",
    pinned: "a_pinned",
    source_turn_id: "a_source_turn_id",
    created_at: "a_created_at",
    last_validated_at: "a_last_validated_at",
    last_accessed_at: "a_last_accessed_at",
    access_count: "a_access_count",
};

const PAIR_COLS_B: PairCols = PairCols {
    id: "b_id",
    agent_id: "b_agent_id",
    kind: "b_kind",
    content: "b_content",
    state: "b_state",
    pinned: "b_pinned",
    source_turn_id: "b_source_turn_id",
    created_at: "b_created_at",
    last_validated_at: "b_last_validated_at",
    last_accessed_at: "b_last_accessed_at",
    access_count: "b_access_count",
};

fn decode_pair_side(
    row: &sqlx::postgres::PgRow,
    cols: &PairCols,
) -> Result<MemoryRow, MemoryStoreError> {
    let access_count_raw: i64 = row.try_get(cols.access_count)?;
    assert!(access_count_raw >= 0);
    let access_count = u64::try_from(access_count_raw).expect("non-negative");
    let content_raw: String = row.try_get(cols.content)?;
    Ok(MemoryRow {
        id: row.try_get(cols.id)?,
        agent_id: row.try_get(cols.agent_id)?,
        kind: row.try_get(cols.kind)?,
        content: MemoryContent::try_from(content_raw)?,
        state: row.try_get(cols.state)?,
        pinned: row.try_get(cols.pinned)?,
        source_turn_id: row.try_get::<Option<PromptRequestId>, _>(cols.source_turn_id)?,
        created_at: row.try_get(cols.created_at)?,
        last_validated_at: row.try_get(cols.last_validated_at)?,
        last_accessed_at: row.try_get(cols.last_accessed_at)?,
        access_count,
    })
}

fn decode_memory_event(row: &sqlx::postgres::PgRow) -> Result<MemoryEvent, MemoryStoreError> {
    let content_before_raw: Option<String> = row.try_get("content_before")?;
    let content_after_raw: Option<String> = row.try_get("content_after")?;
    let source_kind: MutationSourceKind = row.try_get("source_kind")?;
    let source_turn_id: Option<PromptRequestId> = row.try_get("source_turn_id")?;

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

fn decode_contradiction_row(
    row: &sqlx::postgres::PgRow,
) -> Result<ContradictionEventRow, MemoryStoreError> {
    Ok(ContradictionEventRow {
        id: row.try_get("id")?,
        agent_id: row.try_get("agent_id")?,
        memory_a: row.try_get("memory_a")?,
        memory_b: row.try_get("memory_b")?,
        reason: row.try_get("reason")?,
        created_at: row.try_get("created_at")?,
        resolved_at: row.try_get("resolved_at")?,
        resolution_event_id: row.try_get("resolution_event_id")?,
        resolution_reason: row.try_get("resolution_reason")?,
    })
}

/// f64 → f32 narrowing for pgvector cosine similarity. The score is in
/// `[-1, 1]` by construction, well inside f32 range; the assertion is
/// defensive for malformed embeddings.
fn f64_to_f32_clamped(v: f64) -> f32 {
    assert!(
        v.is_nan() || (-2.0..=2.0).contains(&v),
        "invariant: cosine similarity in [-1, 1]; got {v}"
    );
    #[allow(clippy::as_conversions, clippy::cast_possible_truncation)]
    let truncated = v as f32;
    truncated
}
