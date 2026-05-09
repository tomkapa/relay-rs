# Relay web

Frontend for the chat UI. See `doc/frontend_plan.md` for the full plan.

## Dev

In one terminal, run the Rust backend:

```sh
cargo run
```

In another:

```sh
cd web
bun install
bun run dev
```

Open <http://localhost:5173>. The dev server proxies API paths
(`/prompts`, `/agents`, `/requests`, `/threads`, `/mcp-servers`) to the
Rust backend on `:8080`. Override with `BACKEND_URL=...`.

## Build

```sh
bun run build
```

Produces `dist/`. The Rust binary will serve it via `tower-http`'s
`ServeDir` (wiring in the backend, not here).

## Typecheck

```sh
bun run typecheck
```
