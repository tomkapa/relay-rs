import type {
  Agent,
  Me,
  Role,
  SubmitPromptResponse,
  ThreadMessage,
  ThreadSummary,
} from "../types/api";
import { ApiError, AuthRedirect } from "./errors";
import { readCookie } from "./cookies";
import { useAuthStore } from "../stores/authStore";

// Wire-protocol constants — keep in sync with src/auth/limits.rs
// (`CSRF_COOKIE_NAME`, `CSRF_HEADER_NAME`).
const CSRF_COOKIE = "relay_csrf";
const CSRF_HEADER = "X-CSRF-Token";
const SAFE_METHODS = new Set(["GET", "HEAD", "OPTIONS"]);

export async function request<T>(
  path: string,
  init?: RequestInit,
): Promise<T> {
  const method = (init?.method ?? "GET").toUpperCase();
  const headers = new Headers(init?.headers);
  if (!headers.has("content-type")) {
    headers.set("content-type", "application/json");
  }
  if (!SAFE_METHODS.has(method)) {
    const csrf = readCookie(CSRF_COOKIE);
    if (csrf) headers.set(CSRF_HEADER, csrf);
  }
  const res = await fetch(path, { ...init, credentials: "include", headers });

  if (res.status === 401) {
    const back = encodeURIComponent(
      window.location.pathname + window.location.search,
    );
    window.location.href = `/auth/google/login?return_to=${back}`;
    throw new AuthRedirect();
  }

  if (res.status === 403) {
    const body = await res.text().catch(() => "");
    useAuthStore
      .getState()
      .setError({ kind: "forbidden", message: body || undefined });
    throw new ApiError(403, body);
  }

  if (!res.ok) {
    const body = await res.text().catch(() => "");
    throw new ApiError(res.status, body);
  }

  if (res.status === 204) return undefined as T;
  return (await res.json()) as T;
}

export type SwitchOrgResponse = { active_org_id: string; role: Role };

export const api = {
  me: () => request<Me>("/me"),
  switchOrg: (orgId: string) =>
    request<SwitchOrgResponse>("/auth/switch-org", {
      method: "POST",
      body: JSON.stringify({ org_id: orgId }),
    }),
  logout: () => request<void>("/auth/logout", { method: "POST" }),

  agents: () => request<Agent[]>("/agents"),

  threads: () => request<ThreadSummary[]>("/threads"),

  threadMessages: (rootId: string) =>
    request<ThreadMessage[]>(`/threads/${rootId}/messages`),

  submitPrompt: (input: {
    session_id?: string;
    agent_id?: string;
    content: string;
    idempotency_key: string;
  }) =>
    request<SubmitPromptResponse>("/prompts", {
      method: "POST",
      body: JSON.stringify(input),
    }),

  cancelRequest: async (requestId: string) => {
    try {
      await request<void>(`/requests/${requestId}/cancel`, { method: "POST" });
    } catch (e) {
      if (e instanceof ApiError && e.status === 404) return;
      throw e;
    }
  },
};
