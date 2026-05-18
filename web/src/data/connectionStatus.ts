import type { TranslationKey } from "../i18n/en";
import type { ConnectionStatus, McpServer } from "../types/api";

/** Frontend-side tone derived from the backend `connection_status` plus
 *  the `has_credentials` flag. `pending` is synthetic — a server row
 *  with no credentials yet (e.g., custom URL added with `None` auth or
 *  a row mid-OAuth). */
export type StatusTone = "ok" | "reconnect" | "error" | "pending";

const FROM_BACKEND: Record<ConnectionStatus, StatusTone> = {
  ok: "ok",
  reconnect_required: "reconnect",
  error: "error",
};

export function statusToneOf(server: McpServer): StatusTone {
  if (!server.has_credentials) return "pending";
  return FROM_BACKEND[server.connection_status];
}

/** CSS `var(--color-*)` reference per tone. Shared by every surface that
 *  needs to color status text/dot/border. */
export const STATUS_COLOR: Record<StatusTone, string> = {
  ok: "var(--color-moss)",
  reconnect: "var(--color-amber)",
  error: "var(--color-rose)",
  pending: "var(--color-muted-2)",
};

/** Background tint per tone (used by the status pill background fill). */
export const STATUS_BG: Record<StatusTone, string> = {
  ok: "var(--color-moss-tint)",
  reconnect: "var(--color-amber-soft)",
  error: "var(--color-rose-soft)",
  pending: "var(--color-paper-2)",
};

/** Mapping onto the `StatusSquare` atom's variant set. */
export const STATUS_SQUARE: Record<StatusTone, "live" | "idle" | "error" | "muted"> = {
  ok: "live",
  reconnect: "idle",
  error: "error",
  pending: "muted",
};

export const STATUS_KEY: Record<StatusTone, TranslationKey> = {
  ok: "connections.status.ok",
  reconnect: "connections.status.reconnect",
  error: "connections.status.error",
  pending: "connections.status.pending",
};
