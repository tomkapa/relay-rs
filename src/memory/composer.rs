//! Composes the rendered memory section of the system prompt
//! (doc/memory.md §1.3, §2.2 Phase 2).
//!
//! The composer takes the agent's full set of materialized memories and
//! produces:
//!
//! - The *stable layer* — pinned + Self-kind, sorted by state then
//!   recency, trimmed to a byte budget.
//! - The *contextual layer* — top-K Other / Procedure / Open retrieved
//!   against the session's opening context. Phase 9 lands the embedding
//!   provider that drives the actual ranking; Phase 2 stubs this layer to
//!   empty so the renderer's structure is in place when retrieval
//!   arrives.
//!
//! The output carries both the rendered text and a per-section
//! [`MemoryHandleMap`] (`M-NN ↔ MemoryId`) so the agent's tool calls
//! (Phase 3) can be resolved back to UUIDs.

use std::collections::HashMap;
use std::sync::Arc;

use super::limits::{
    CONTEXTUAL_LAYER_MAX_BYTES, RENDER_LINE_OVERHEAD_BYTES, STABLE_LAYER_MAX_BYTES,
};
use super::store::MemoryRow;
use super::types::{MemoryHandle, MemoryId, MemoryKind};

/// Stable XML-ish tags wrapping the rendered memory section. Matches the
/// existing `<core>...</core>` / `<role>...</role>` framing so the model
/// sees one consistent envelope per system block.
pub const MEMORY_TAG_OPEN: &str = "<memory>\n";
pub const MEMORY_TAG_CLOSE: &str = "\n</memory>";

/// Order section headers in the rendered output. Pinned-or-Self memories
/// can land in any kind, but kinds appear in this fixed sequence so
/// section structure stays stable across compositions.
const KIND_ORDER: &[MemoryKind] = &[
    MemoryKind::Identity,
    MemoryKind::Other,
    MemoryKind::Procedure,
    MemoryKind::Open,
];

/// Resolved (handle ↔ id) map for one session's composed memory section.
///
/// Frozen for the session's lifetime alongside the rendered text.
/// Phase 3's mutation tools call [`Self::resolve`] on every `M-NN`
/// argument the model sends back.
#[derive(Debug, Clone, Default)]
pub struct MemoryHandleMap {
    by_handle: HashMap<MemoryHandle, MemoryId>,
}

impl MemoryHandleMap {
    #[must_use]
    pub fn resolve(&self, handle: MemoryHandle) -> Option<MemoryId> {
        self.by_handle.get(&handle).copied()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.by_handle.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_handle.is_empty()
    }
}

/// Rendered memory section + the handle map the rendering produced.
///
/// `text` includes the surrounding `<memory>...</memory>` envelope
/// when non-empty; empty memory yields an empty `text` so callers can
/// append it unconditionally. Fields are encapsulated; access through
/// [`Self::text`] / [`Self::handles`].
#[derive(Debug, Clone)]
pub struct MemorySection {
    text: Arc<str>,
    handles: MemoryHandleMap,
}

impl MemorySection {
    #[must_use]
    pub fn empty() -> Self {
        Self {
            text: Arc::from(""),
            handles: MemoryHandleMap::default(),
        }
    }

    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    #[must_use]
    pub fn handles(&self) -> &MemoryHandleMap {
        &self.handles
    }

    /// `text` is empty iff the handle map is empty (the renderer only
    /// produces an envelope when at least one row was rendered, and
    /// every rendered row contributes a handle). Either check is
    /// equivalent; we key off the handle map since it is structurally
    /// the smaller invariant.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.handles.is_empty()
    }
}

/// Compose the memory section from the agent's full row set.
///
/// `rows` is the entire materialized table for the agent (capped per
/// agent at [`super::limits::MAX_MEMORIES_PER_AGENT`]); the composer
/// filters into the two layers. Phase 9 will add an `opener: &str`
/// parameter for the contextual retrieval; today it is unused.
#[must_use]
pub fn compose_memory_section(rows: &[MemoryRow]) -> MemorySection {
    let stable = select_stable_layer(rows);
    let contextual = select_contextual_layer(rows);
    render(&stable, &contextual)
}

/// Stable layer per doc/memory.md §1.3: pinned + Self-kind, sorted by
/// state (`Core > Validated > Held > Tentative`) then by created_at
/// descending. Trimmed to [`STABLE_LAYER_MAX_BYTES`].
fn select_stable_layer(rows: &[MemoryRow]) -> Vec<&MemoryRow> {
    let mut candidates: Vec<&MemoryRow> = rows
        .iter()
        .filter(|r| r.pinned || r.kind == MemoryKind::Identity)
        .collect();
    candidates.sort_by(|a, b| {
        state_priority(b.state)
            .cmp(&state_priority(a.state))
            .then_with(|| b.created_at.cmp(&a.created_at))
    });
    trim_to_budget(candidates, STABLE_LAYER_MAX_BYTES)
}

/// Contextual layer per doc/memory.md §1.3: top-K embedding-retrieved
/// from Other / Procedure / Open. Phase 2 stub returns empty — Phase 9
/// adds the embedding provider and replaces this body.
fn select_contextual_layer(_rows: &[MemoryRow]) -> Vec<&MemoryRow> {
    Vec::new()
}

/// Per-state priority used by the stable-layer sort. Higher = more
/// trusted, surfaces first in the rendered prompt.
const fn state_priority(state: super::types::MemoryState) -> u8 {
    match state {
        super::types::MemoryState::Core => 4,
        super::types::MemoryState::Validated => 3,
        super::types::MemoryState::Held => 2,
        super::types::MemoryState::Tentative => 1,
    }
}

fn trim_to_budget(rows: Vec<&MemoryRow>, budget: usize) -> Vec<&MemoryRow> {
    let mut total = 0usize;
    let mut kept = Vec::with_capacity(rows.len());
    for row in rows {
        let line = row.content.len() + RENDER_LINE_OVERHEAD_BYTES;
        if total.saturating_add(line) > budget {
            break;
        }
        total += line;
        kept.push(row);
    }
    kept
}

fn render(stable: &[&MemoryRow], contextual: &[&MemoryRow]) -> MemorySection {
    if stable.is_empty() && contextual.is_empty() {
        return MemorySection::empty();
    }

    let mut by_kind: HashMap<MemoryKind, Vec<(MemoryHandle, &MemoryRow)>> = HashMap::new();
    let mut handles = HashMap::new();
    let mut next_handle: u32 = 1;

    for row in stable.iter().chain(contextual.iter()) {
        let handle = MemoryHandle::try_from(next_handle)
            .expect("invariant: handle count under cap (per-agent quota enforces it)");
        next_handle = next_handle
            .checked_add(1)
            .expect("invariant: handle counter cannot overflow under per-agent quota");
        handles.insert(handle, row.id);
        by_kind.entry(row.kind).or_default().push((handle, *row));
    }

    let estimated = STABLE_LAYER_MAX_BYTES + CONTEXTUAL_LAYER_MAX_BYTES + 64;
    let mut buf = String::with_capacity(estimated);
    buf.push_str(MEMORY_TAG_OPEN);
    buf.push_str("## Memory\n");

    for kind in KIND_ORDER {
        let Some(entries) = by_kind.get(kind) else {
            continue;
        };
        if entries.is_empty() {
            continue;
        }
        buf.push('\n');
        buf.push_str("### ");
        buf.push_str(kind.display_label());
        buf.push('\n');
        for (handle, row) in entries {
            // `- [<handle>, <state>] <content>` per doc/memory.md §1.3.
            buf.push_str("- [");
            buf.push_str(&handle.to_string());
            buf.push_str(", ");
            buf.push_str(row.state.as_str());
            buf.push_str("] ");
            buf.push_str(row.content.as_str());
            buf.push('\n');
        }
    }

    buf.push_str(MEMORY_TAG_CLOSE);

    MemorySection {
        text: Arc::from(buf),
        handles: MemoryHandleMap { by_handle: handles },
    }
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};

    use crate::agents::AgentId;
    use crate::memory::store::MemoryRow;
    use crate::memory::types::{MemoryContent, MemoryKind, MemoryState};

    use super::*;

    fn row(
        ix: u8,
        kind: MemoryKind,
        content: &str,
        state: MemoryState,
        pinned: bool,
        ts_secs: i64,
    ) -> MemoryRow {
        MemoryRow {
            id: MemoryId::new(),
            agent_id: AgentId::new(),
            kind,
            content: MemoryContent::try_from(content).expect("valid"),
            state,
            pinned,
            source_turn_id: None,
            created_at: Utc.timestamp_opt(ts_secs, 0).unwrap(),
            last_validated_at: Utc.timestamp_opt(ts_secs, 0).unwrap(),
            last_accessed_at: Utc.timestamp_opt(ts_secs, 0).unwrap(),
            access_count: u64::from(ix),
        }
    }

    #[test]
    fn empty_input_renders_empty_section() {
        let section = compose_memory_section(&[]);
        assert!(section.is_empty());
        assert!(section.handles().is_empty());
    }

    #[test]
    fn stable_layer_filters_to_pinned_or_self() {
        let rows = vec![
            row(
                1,
                MemoryKind::Identity,
                "self memory",
                MemoryState::Held,
                false,
                100,
            ),
            row(
                2,
                MemoryKind::Other,
                "other memory",
                MemoryState::Held,
                false,
                200,
            ),
            row(
                3,
                MemoryKind::Procedure,
                "pinned proc",
                MemoryState::Core,
                true,
                300,
            ),
        ];
        let section = compose_memory_section(&rows);
        let txt = section.text();
        assert!(txt.contains("self memory"), "Self kind included: {txt}");
        assert!(
            txt.contains("pinned proc"),
            "pinned non-Self included: {txt}"
        );
        assert!(
            !txt.contains("other memory"),
            "non-pinned non-Self excluded: {txt}"
        );
    }

    #[test]
    fn stable_layer_sorted_by_state_then_recency() {
        let rows = vec![
            row(
                1,
                MemoryKind::Identity,
                "tentative-old",
                MemoryState::Tentative,
                false,
                100,
            ),
            row(
                2,
                MemoryKind::Identity,
                "validated-old",
                MemoryState::Validated,
                false,
                100,
            ),
            row(
                3,
                MemoryKind::Identity,
                "held-new",
                MemoryState::Held,
                false,
                999,
            ),
            row(
                4,
                MemoryKind::Identity,
                "validated-new",
                MemoryState::Validated,
                false,
                500,
            ),
        ];
        let section = compose_memory_section(&rows);
        let txt = section.text();
        let pos = |s: &str| txt.find(s).expect("present");
        // Validated > Held > Tentative; within Validated the newer one first.
        assert!(pos("validated-new") < pos("validated-old"));
        assert!(pos("validated-old") < pos("held-new"));
        assert!(pos("held-new") < pos("tentative-old"));
    }

    #[test]
    fn handle_map_round_trips_to_memory_id() {
        let rows = vec![row(
            1,
            MemoryKind::Identity,
            "first",
            MemoryState::Held,
            false,
            100,
        )];
        let section = compose_memory_section(&rows);
        let id = rows[0].id;
        let handle = MemoryHandle::try_from(1u32).expect("valid");
        assert_eq!(section.handles().resolve(handle), Some(id));
        assert!(section.text().contains("M-1"));
    }

    #[test]
    fn render_uses_state_label_and_handle() {
        let rows = vec![row(
            1,
            MemoryKind::Identity,
            "terse replies",
            MemoryState::Validated,
            false,
            100,
        )];
        let section = compose_memory_section(&rows);
        assert!(
            section.text().contains("- [M-1, validated] terse replies"),
            "render shape: {}",
            section.text()
        );
        assert!(section.text().contains("### Self"));
        assert!(
            section.text().starts_with(MEMORY_TAG_OPEN),
            "wrapped in <memory>: {}",
            section.text()
        );
        assert!(section.text().ends_with(MEMORY_TAG_CLOSE));
    }

    #[test]
    fn trim_drops_overflow_past_budget() {
        // Each row is ~CONTENT bytes; with overhead, fit only a handful
        // before tripping the cap. Use a small content size and many rows
        // to force the trim.
        let big_content = "x".repeat(STABLE_LAYER_MAX_BYTES / 4);
        let rows: Vec<MemoryRow> = (0..10)
            .map(|i| {
                row(
                    i,
                    MemoryKind::Identity,
                    big_content.as_str(),
                    MemoryState::Held,
                    false,
                    i64::from(i),
                )
            })
            .collect();
        let section = compose_memory_section(&rows);
        // Must include at least one row, but fewer than all 10.
        let count = section.text().matches("- [M-").count();
        assert!(count >= 1, "at least one row kept");
        assert!(count < rows.len(), "trimmed below row count");
    }

    #[test]
    fn kinds_render_in_fixed_order() {
        // Two pinned memories, one Other (later by recency), one
        // Procedure (earlier). Render order must be Other → Procedure
        // regardless of recency, since kind ordering is fixed.
        let rows = vec![
            row(
                1,
                MemoryKind::Procedure,
                "pin-proc",
                MemoryState::Core,
                true,
                100,
            ),
            row(
                2,
                MemoryKind::Other,
                "pin-other",
                MemoryState::Core,
                true,
                200,
            ),
        ];
        let section = compose_memory_section(&rows);
        let txt = section.text();
        let other = txt.find("### Other").expect("### Other present");
        let proc = txt.find("### Procedure").expect("### Procedure present");
        assert!(other < proc, "Other section precedes Procedure: {txt}");
    }
}
