// Pure helpers for the per-agent allowlist editor — kept out of the
// React components so they can be reasoned about in isolation. The wire
// shape (`Record<serverId, string[] | null>`) is preserved exactly:
//   - key absent          → no access
//   - value === null      → all tools (the cheap "checked the parent" default)
//   - value === string[]  → only those remote tool names
// Helpers normalize the shape on every mutation so the editor never
// emits redundant arrays the backend would semantically equate to `null`.

export type Allowlist = Record<string, string[] | null>;

export type ServerCheckState = "unchecked" | "mixed" | "all";

export function shapeOf(
  list: Allowlist,
  serverId: string,
  knownTools: readonly string[],
): ServerCheckState {
  const v = list[serverId];
  if (v === undefined) return "unchecked";
  if (v === null) return "all";
  // Filter against the live tool catalog before counting — a stale name
  // (e.g. a tool removed from the connection between sessions) must not
  // promote the row to "all", which would over-grant access on the next
  // save by collapsing back to `null`.
  const known = new Set(knownTools);
  const selected = v.filter((t) => known.has(t)).length;
  if (selected === 0) return "unchecked";
  if (knownTools.length > 0 && selected === knownTools.length) return "all";
  return "mixed";
}

export function isToolAllowed(
  list: Allowlist,
  serverId: string,
  toolName: string,
): boolean {
  const v = list[serverId];
  if (v === undefined) return false;
  if (v === null) return true;
  return v.includes(toolName);
}

export function toggleServer(
  list: Allowlist,
  serverId: string,
  next: boolean,
): Allowlist {
  if (next) return { ...list, [serverId]: null };
  const { [serverId]: _omit, ...rest } = list;
  return rest;
}

/** Toggle a single tool. Collapses to `null` when every known tool is
 *  selected and prunes the parent key entirely when nothing remains —
 *  the backend treats `null`, `string[]`, and absence as three distinct
 *  states, so the editor never emits the empty-array shape. */
export function toggleTool(
  list: Allowlist,
  serverId: string,
  toolName: string,
  knownTools: readonly string[],
  next: boolean,
): Allowlist {
  const known = new Set(knownTools);
  if (!known.has(toolName)) return list;
  const current = list[serverId];
  // Start from the explicit set: `null` expands to every known tool so
  // unchecking one yields a shrunk array, not "all minus one + null".
  // An existing `string[]` is filtered against the live catalog so stale
  // names can't promote the row to "all" via the length-equality check below.
  const base =
    current === undefined
      ? []
      : current === null
        ? [...knownTools]
        : current.filter((t) => known.has(t));
  let updated = next
    ? base.includes(toolName)
      ? base
      : [...base, toolName]
    : base.filter((t) => t !== toolName);

  if (updated.length === 0) {
    const { [serverId]: _omit, ...rest } = list;
    return rest;
  }
  if (knownTools.length > 0 && updated.length === knownTools.length) {
    return { ...list, [serverId]: null };
  }
  // Keep the array stable-sorted by `knownTools` order so saves are
  // deterministic and diff-friendly across renders.
  const order = new Map(knownTools.map((t, i) => [t, i] as const));
  updated.sort((a, b) => (order.get(a) ?? 0) - (order.get(b) ?? 0));
  return { ...list, [serverId]: updated };
}

/** Total of every tool the agent can currently invoke across all
 *  enabled connections. `null` value counts as `knownTools.length`.
 *  Explicit tool names are filtered against the live catalog so a stale
 *  entry doesn't inflate the count. */
export function totalToolsAllowed(
  list: Allowlist,
  toolsByServer: Record<string, readonly string[]>,
): number {
  let n = 0;
  for (const [sid, v] of Object.entries(list)) {
    const known = toolsByServer[sid] ?? [];
    if (v === null) {
      n += known.length;
    } else {
      const knownSet = new Set(known);
      n += v.filter((t) => knownSet.has(t)).length;
    }
  }
  return n;
}

export function allowlistsEqual(a: Allowlist, b: Allowlist): boolean {
  // Stringify with sorted keys; the editor never emits empty arrays so
  // shape equality coincides with byte equality.
  return JSON.stringify(sortObject(a)) === JSON.stringify(sortObject(b));
}

function sortObject(obj: Allowlist): Allowlist {
  const out: Allowlist = {};
  for (const key of Object.keys(obj).sort()) {
    out[key] = obj[key]!;
  }
  return out;
}
