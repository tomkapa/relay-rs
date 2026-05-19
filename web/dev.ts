import index from "./index.html";

const BACKEND = process.env.BACKEND_URL ?? "http://localhost:8080";

const proxyToBackend = (req: Request): Promise<Response> => {
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

// Routes like `/agents/:id` collide between the SPA (browser navigation
// wants `index.html`) and the BE (XHR wants the JSON API). Discriminate
// by `Accept`: HTML navigations land on the SPA shell; everything else
// (XHR / fetch with `application/json`) hits the proxy. This mirrors how
// production handles the same paths via the BE serving HTML on misses.
const proxy = async (req: Request): Promise<Response> => {
  const accept = req.headers.get("accept") ?? "";
  const method = req.method.toUpperCase();
  const looksLikeNavigation =
    method === "GET" && accept.includes("text/html");
  if (looksLikeNavigation) {
    return new Response(Bun.file("./index.html"), {
      headers: { "content-type": "text/html; charset=utf-8" },
    });
  }
  return proxyToBackend(req);
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
    "/auth/google/login": proxyToBackend,
    "/auth/google/callback": proxyToBackend,
    "/auth/switch-org": proxy,
    "/auth/logout": proxy,
    "/*": index,
  },
});

console.log(`web dev → http://localhost:${server.port}`);
console.log(`proxy   → ${BACKEND}`);
