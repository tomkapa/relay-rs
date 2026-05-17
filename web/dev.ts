import index from "./index.html";

const BACKEND = process.env.BACKEND_URL ?? "http://localhost:8080";

const proxy = (req: Request): Promise<Response> => {
  const url = new URL(req.url);
  const target = `${BACKEND}${url.pathname}${url.search}`;
  return fetch(target, {
    method: req.method,
    headers: req.headers,
    body: req.body,
    redirect: "manual",
    // @ts-expect-error - duplex is required for streaming bodies in fetch
    duplex: "half",
  });
};

const server = Bun.serve({
  port: 5173,
  development: true,
  routes: {
    "/prompts": proxy,
    "/prompts/*": proxy,
    "/agents": proxy,
    "/agents/*": proxy,
    "/requests": proxy,
    "/requests/*": proxy,
    "/threads": proxy,
    "/threads/*": proxy,
    "/mcp-servers": proxy,
    "/mcp-servers/*": proxy,
    "/me": proxy,
    "/auth/google/login": proxy,
    "/auth/google/callback": proxy,
    "/auth/switch-org": proxy,
    "/auth/logout": proxy,
    "/*": index,
  },
});

console.log(`web dev → http://localhost:${server.port}`);
console.log(`proxy   → ${BACKEND}`);
