import type {
  Agent,
  SubmitPromptResponse,
  ThreadMessage,
  ThreadSummary,
} from "../types/api";

async function jsonOk<T>(res: Response): Promise<T> {
  if (!res.ok) {
    const body = await res.text().catch(() => "");
    throw new Error(`HTTP ${res.status} ${res.statusText}: ${body}`);
  }
  return res.json() as Promise<T>;
}

export const api = {
  agents: () => fetch("/agents").then(jsonOk<Agent[]>),

  threads: () => fetch("/threads").then(jsonOk<ThreadSummary[]>),

  threadMessages: (rootId: string) =>
    fetch(`/threads/${rootId}/messages`).then(jsonOk<ThreadMessage[]>),

  submitPrompt: (input: {
    session_id?: string;
    agent_id?: string;
    content: string;
    idempotency_key: string;
  }) =>
    fetch("/prompts", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(input),
    }).then(jsonOk<SubmitPromptResponse>),

  cancelRequest: (requestId: string) =>
    fetch(`/requests/${requestId}/cancel`, { method: "POST" }).then((r) => {
      if (!r.ok && r.status !== 404)
        throw new Error(`HTTP ${r.status} ${r.statusText}`);
    }),
};
