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
