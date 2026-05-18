// Tiny in-memory mock backend used only for visual verification of the
// Connections UI against the design.pen frames. Not wired into prod —
// run it manually when you want to drive `/connections*` without the
// real Rust server.
//
//   BACKEND_PORT=8081 bun mock-backend.ts &
//   BACKEND_URL=http://localhost:8081 bun dev.ts
//
// Routes only cover what the Connections pages call: /me, /mcp-servers,
// /mcp-servers/{id}, /mcp-servers/{id}/oauth/start,
// /mcp-servers/test-connect, /mcp-oauth/callback (echo-redirect).

const PORT = Number(process.env.BACKEND_PORT ?? 8081);

type Server = {
  id: string;
  alias: string;
  enabled: boolean;
  config: { kind: "http"; url: string };
  description: string | null;
  last_seen_at: string | null;
  last_error: string | null;
  discovered_tools: { prefixed_name: string; remote_name: string; description: string | null }[] | null;
  created_by_user_id: string;
  has_credentials: boolean;
  credentials_kind: "static_headers" | "oauth2" | null;
  connection_status: "ok" | "reconnect_required" | "error";
  created_at: string;
  updated_at: string;
};

const USER_ID = "00000000-0000-7000-8000-000000000001";
const ORG_ID = "00000000-0000-7000-8000-000000000aaa";

const NOW = new Date().toISOString();
const MIN_2_AGO = new Date(Date.now() - 2 * 60 * 1000).toISOString();
const MIN_14_AGO = new Date(Date.now() - 14 * 60 * 1000).toISOString();
const DAY_3_AGO = new Date(Date.now() - 3 * 24 * 60 * 60 * 1000).toISOString();
const WEEK_1_AGO = new Date(Date.now() - 7 * 24 * 60 * 60 * 1000).toISOString();

const seed: Server[] = [
  {
    id: "11111111-1111-7111-8111-111111111111",
    alias: "notion",
    enabled: true,
    config: { kind: "http", url: "https://mcp.notion.com/mcp" },
    description: "Notion",
    last_seen_at: MIN_2_AGO,
    last_error: null,
    discovered_tools: Array.from({ length: 12 }, (_, i) => ({
      prefixed_name: `mcp_notion_t${i}`,
      remote_name: `t${i}`,
      description: null,
    })),
    created_by_user_id: USER_ID,
    has_credentials: true,
    credentials_kind: "oauth2",
    connection_status: "ok",
    created_at: DAY_3_AGO,
    updated_at: NOW,
  },
  {
    id: "22222222-2222-7222-8222-222222222222",
    alias: "linear",
    enabled: true,
    config: { kind: "http", url: "https://mcp.linear.app/sse" },
    description: "Linear",
    last_seen_at: MIN_14_AGO,
    last_error: "401 unauthorized",
    discovered_tools: Array.from({ length: 8 }, (_, i) => ({
      prefixed_name: `mcp_linear_t${i}`,
      remote_name: `t${i}`,
      description: null,
    })),
    created_by_user_id: USER_ID,
    has_credentials: true,
    credentials_kind: "oauth2",
    connection_status: "reconnect_required",
    created_at: WEEK_1_AGO,
    updated_at: NOW,
  },
  {
    id: "33333333-3333-7333-8333-333333333333",
    alias: "slack",
    enabled: false,
    config: { kind: "http", url: "https://mcp.slack.com/v1" },
    description: "Slack",
    last_seen_at: DAY_3_AGO,
    last_error: null,
    discovered_tools: Array.from({ length: 9 }, (_, i) => ({
      prefixed_name: `mcp_slack_t${i}`,
      remote_name: `t${i}`,
      description: null,
    })),
    created_by_user_id: USER_ID,
    has_credentials: true,
    credentials_kind: "oauth2",
    connection_status: "ok",
    created_at: WEEK_1_AGO,
    updated_at: NOW,
  },
  {
    id: "44444444-4444-7444-8444-444444444444",
    alias: "github",
    enabled: true,
    config: { kind: "http", url: "https://api.githubcopilot.com/mcp/" },
    description: "GitHub",
    last_seen_at: null,
    last_error: "ECONNRESET",
    discovered_tools: null,
    created_by_user_id: USER_ID,
    has_credentials: true,
    credentials_kind: "static_headers",
    connection_status: "error",
    created_at: WEEK_1_AGO,
    updated_at: NOW,
  },
  {
    id: "55555555-5555-7555-8555-555555555555",
    alias: "internal-search",
    enabled: false,
    config: { kind: "http", url: "https://search.acme.internal/mcp" },
    description: "Internal search",
    last_seen_at: null,
    last_error: null,
    discovered_tools: null,
    created_by_user_id: USER_ID,
    has_credentials: false,
    credentials_kind: null,
    connection_status: "ok",
    created_at: NOW,
    updated_at: NOW,
  },
];

const servers = new Map<string, Server>(seed.map((s) => [s.id, s]));

const me = {
  user: {
    id: USER_ID,
    email: "alice@example.com",
    display_name: "Alice",
    avatar_url: null,
  },
  orgs: [
    {
      id: ORG_ID,
      name: "Acme",
      slug: "acme",
      role: "owner" as const,
      default_language: "en" as const,
    },
  ],
  active_org_id: ORG_ID,
  role: "owner" as const,
};

const json = (body: unknown, status = 200) =>
  new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  });

const empty = (status = 204) => new Response(null, { status });

function maybeOAuthStart(id: string): Response {
  // Mock authorize_url just bounces to the fake callback success — gives
  // the FE a usable round-trip without a vendor. We also simulate the
  // real backend's post-callback state mutation: flip the server's
  // connection_status to `ok` and mark credentials as present.
  const s = servers.get(id);
  if (s) {
    servers.set(id, {
      ...s,
      connection_status: "ok",
      has_credentials: true,
      credentials_kind: s.credentials_kind ?? "oauth2",
      last_error: null,
    });
  }
  const base = process.env.MOCK_FRONTEND_BASE ?? "http://localhost:5173";
  const callback = `${base}/connections/oauth-callback?server_id=${id}&status=ok`;
  return json({ authorize_url: callback });
}

const server = Bun.serve({
  port: PORT,
  async fetch(req) {
    const url = new URL(req.url);
    const path = url.pathname;
    const method = req.method.toUpperCase();

    if (path === "/me" && method === "GET") return json(me);

    if (path === "/mcp-servers" && method === "GET") {
      return json([...servers.values()].sort((a, b) => a.alias.localeCompare(b.alias)));
    }

    if (path === "/mcp-servers" && method === "POST") {
      const body = (await req.json()) as {
        alias: string;
        config: { kind: "http"; url: string };
        description?: string | null;
        enabled?: boolean;
        credentials?: { kind: "static_headers"; headers: Record<string, string> };
      };
      const id = `mock-${crypto.randomUUID()}`;
      const now = new Date().toISOString();
      const created: Server = {
        id,
        alias: body.alias,
        enabled: body.enabled ?? true,
        config: body.config,
        description: body.description ?? null,
        last_seen_at: null,
        last_error: null,
        discovered_tools: null,
        created_by_user_id: USER_ID,
        has_credentials: Boolean(body.credentials),
        credentials_kind: body.credentials ? "static_headers" : null,
        connection_status: "ok",
        created_at: now,
        updated_at: now,
      };
      servers.set(id, created);
      return json(created, 201);
    }

    if (path === "/mcp-servers/test-connect" && method === "POST") {
      return json({ outcome: "ok", discovered_tools: [] });
    }

    const mcpMatch = path.match(/^\/mcp-servers\/([^/]+)(\/.*)?$/);
    if (mcpMatch) {
      const id = mcpMatch[1]!;
      const sub = mcpMatch[2] ?? "";
      const s = servers.get(id);

      if (sub === "" && method === "GET") {
        return s ? json(s) : empty(404);
      }
      if (sub === "" && method === "PUT") {
        if (!s) return empty(404);
        const body = (await req.json()) as Partial<Server>;
        const next: Server = { ...s, ...body, id, updated_at: new Date().toISOString() };
        servers.set(id, next);
        return json(next);
      }
      if (sub === "" && method === "DELETE") {
        servers.delete(id);
        return empty(204);
      }
      if (sub === "/credentials" && method === "PUT") {
        if (s) servers.set(id, { ...s, has_credentials: true, credentials_kind: "static_headers" });
        return empty(204);
      }
      if (sub === "/credentials" && method === "DELETE") {
        if (s) servers.set(id, { ...s, has_credentials: false, credentials_kind: null });
        return empty(204);
      }
      if (sub === "/oauth/start" && method === "POST") return maybeOAuthStart(id);
      if (sub === "/oauth/disconnect" && method === "POST") {
        if (s) servers.set(id, { ...s, has_credentials: false, credentials_kind: null });
        return json({ ok: true });
      }
    }

    if (path === "/mcp-oauth/callback" && method === "GET") {
      const qs = url.searchParams;
      const dest = qs.get("status") === "failed"
        ? `/connections/oauth-callback?status=failed&reason=${qs.get("reason") ?? "unknown"}`
        : `/connections/oauth-callback?server_id=${qs.get("server_id") ?? ""}&status=ok`;
      return new Response(null, { status: 303, headers: { location: dest } });
    }

    if (path === "/auth/switch-org" && method === "POST") {
      return json({ active_org_id: ORG_ID, role: "owner" });
    }

    return empty(404);
  },
});

console.log(`mock backend → http://localhost:${server.port}`);
