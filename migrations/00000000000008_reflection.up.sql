-- Reflection support (doc/memory.md §1.6, §2.4 — Phase 4).
--
-- Two changes:
--
-- 1. `agents.reflection_role` — optional per-agent guidance read by the
--    reflection composer ("reflect the way *this* agent reflects").
--    Same length cap as `system_prompt`; `NULL` is the no-op default.
-- 2. `reflection_checkpoints.reflection_event_id` becomes nullable.
--    A reflection that produces zero memory mutations still records its
--    checkpoint so the next sweep advances; without nullability we would
--    have to mint a synthetic "no-op" journal event just to satisfy the
--    FK. Pre-launch — see `feedback_no_backcompat`: no rows exist yet,
--    so this is a one-shot relax with no migration churn.

ALTER TABLE agents
    ADD COLUMN reflection_role TEXT NULL
        CHECK (reflection_role IS NULL
               OR octet_length(reflection_role) BETWEEN 1 AND 16384);

ALTER TABLE reflection_checkpoints
    ALTER COLUMN reflection_event_id DROP NOT NULL;
