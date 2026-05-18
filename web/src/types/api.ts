// Wire types mirror src/runtime/response.rs and the route handlers.
// Keep in sync with: src/http/routes/threads.rs, src/runtime/response.rs.

export type Role = "owner" | "admin" | "member";

/** Mirrors `src/auth/language.rs` — kept narrow so the i18n layer can
 *  exhaustive-match on it. Adding a language here requires a paired
 *  backend change (new TOML + migration CHECK update). */
export type Language = "en" | "vi";

export type Org = {
  id: string;
  name: string;
  slug: string;
  role: Role;
  /** Org-wide language driving the agent's `<language>` directive and
   *  the web app's i18n. Mutated via `PATCH /me/org/language` for
   *  owner/admin members. */
  default_language: Language;
};

export type User = {
  id: string;
  email: string;
  display_name: string | null;
  avatar_url: string | null;
};

export type Me = {
  user: User;
  orgs: Org[];
  active_org_id: string;
  role: Role;
};

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

// ─── MCP server wire shapes ─────────────────────────────────────────────
// Mirrors src/http/routes/mcp.rs and src/mcp/types.rs. Adding a transport
// kind or credential kind requires a paired backend change.

export type McpTransport = { kind: "http"; url: string };

/** Mirrors `src/mcp/types.rs::ConnectionStatus`. */
export type ConnectionStatus = "ok" | "reconnect_required" | "error";

/** Per-tool discovery summary surfaced in McpServer.discovered_tools. */
export type DiscoveredTool = {
  prefixed_name: string;
  remote_name: string;
  description: string | null;
};

export type McpServer = {
  id: string;
  alias: string;
  enabled: boolean;
  config: McpTransport;
  description: string | null;
  last_seen_at: string | null;
  last_error: string | null;
  discovered_tools: DiscoveredTool[] | null;
  created_by_user_id: string;
  has_credentials: boolean;
  credentials_kind: "static_headers" | "oauth2" | null;
  connection_status: ConnectionStatus;
  created_at: string;
  updated_at: string;
};

/** Only `static_headers` is accepted on the create/replace path today;
 *  OAuth tokens are written by the callback handler. */
export type CredentialInput = {
  kind: "static_headers";
  headers: Record<string, string>;
};

export type CreateMcpServerRequest = {
  alias: string;
  config: McpTransport;
  description?: string | null;
  enabled?: boolean;
  credentials?: CredentialInput;
};

export type UpdateMcpServerRequest = {
  alias?: string;
  config?: McpTransport;
  description?: string | null;
  enabled?: boolean;
};

export type TestConnectRequest = {
  config: McpTransport;
  credentials?: CredentialInput;
};

export type TestConnectResponse =
  | { outcome: "ok"; discovered_tools: DiscoveredTool[] }
  | { outcome: "failed"; error: string };

export type OAuthStartRequest = {
  redirect_to?: string;
  scope?: string;
};

export type OAuthStartResponse = { authorize_url: string };
