// Wire types mirror src/runtime/response.rs and the route handlers.
// Keep in sync with: src/http/routes/threads.rs, src/runtime/response.rs.

export type AgentRef = { id: string; name: string };

export type Agent = {
  id: string;
  name: string;
  is_default: boolean;
};

export type RequestStatus = "pending" | "processing" | "done" | "failed";

export type ThreadSummary = {
  root_request_id: string;
  root_session_id: string;
  first_agent: AgentRef;
  preview: string;
  reply_count: number;
  last_activity_at: string;
  status: RequestStatus;
  created_at: string;
};

export type Participant =
  | { kind: "human" }
  | { kind: "agent"; agent_id: string }
  | { kind: "system" };

// Mirrors src/provider/chat.rs `ChatMessage` + UserContent / AssistantContent.
// Wire shape is `{role, contents: [{kind, value}]}`; the demo fixtures tolerate
// the legacy `{role, content: string}` form too.
export type ContentBlock =
  | { kind: "text"; value: string }
  | { kind: "reasoning"; value: string }
  | {
      kind: "tool_call";
      value: { id: string; name: string; input: unknown };
    }
  | {
      kind: "tool_result";
      value: { call_id: string; output: string; is_error?: boolean };
    };

export type ChatMessageBody = {
  role?: "user" | "assistant" | "system" | "tool";
  contents?: ContentBlock[];
  /** Legacy / demo shorthand. */
  content?: string;
  [k: string]: unknown;
};

export type ThreadMessage = {
  session_id: string;
  seq: number;
  sender: Participant;
  receiver: Participant;
  body: ChatMessageBody;
  created_at: string;
  /** The prompt request that produced this row. The thread panel uses it to
   *  reconcile optimistic / live / persisted bubbles by identity instead of
   *  by text matching. */
  request_id: string;
};

// ─── ResponseChunk wire shapes ──────────────────────────────────────────

export type ToolCallPayload = {
  id: string;
  name: string;
  input: unknown;
};

export type ToolResultPayload = {
  call_id: string;
  output: string;
  is_error?: boolean;
};

export type ResponseChunk =
  | { kind: "text"; value: string }
  | { kind: "reasoning"; value: string }
  | { kind: "tool_call"; id: string; name: string; input: unknown }
  | { kind: "tool_result"; call_id: string; output: string; is_error?: boolean }
  | { kind: "agent_message"; from: string; content: string }
  | { kind: "done"; final_text: string }
  | { kind: "error"; reason: string }
  | { kind: "stalled" };

export type ToolCallEntry = {
  call_id: string;
  name: string;
  input?: unknown;
  output?: string;
  is_error?: boolean;
  status: "running" | "ok" | "error";
};

export type ThreadStreamEnvelope = {
  request_id: string | null;
  from_agent: string | null;
  chunk_seq: number | null;
  chunk: ResponseChunk;
};

export type SubmitPromptResponse = {
  request_id: string;
  session_id: string;
  status: RequestStatus;
};
