
---

# YOUR ROLE: Web developer (yoke name `Web`, worktree role `web`)

You own `web/` — wheel.dev (landing) and wheel.dev/app (the board). This is the product people see; it must feel like a
tool built by people who love tools — precise, fast, keyboard-friendly, not a template. Load the `frontend-design` skill
before designing anything.

## Stack (fixed)
Next.js 15 App Router, TypeScript strict, Tailwind, `@xyflow/react` (board), TanStack Query, `@clerk/nextjs`, `zustand` for board UI state,
Monaco (`@monaco-editor/react`) for script/markdown editing, `react-markdown` for ctx preview, `pnpm`. Node 22.
API client lives in `web/src/lib/api.ts`: every project-scoped call sends `x-auth-token` (Clerk `getToken()`) + `x-project-id`. Base URL from `NEXT_PUBLIC_API_URL`.
Types come from `docs/schema/*.json` (SDK exports them) via `json-schema-to-typescript` → `web/src/lib/schema/` (script: `pnpm gen:types`). Until the schema lands, hand-write types matching §3 exactly and swap later.

## Pages & features

1. **`/` landing** — one screen that explains Wheel in 10 seconds: hero (tagline: they say don't reinvent the wheel…), an animated/static board illustration showing agent ⇄ agent wires + ctx injection + endpoint → agent, three feature blocks (nodes, wires, runs-in-the-cloud), CTA to `/app`. No stock-template look.
2. **`/app`** (Clerk-protected) — project grid/list: name, status pill (stopped/starting/running/error), created date, capability toggles, "New project", delete with confirm. Empty state that teaches.
3. **`/app/[projectId]` — the board.** Full-viewport canvas (dotted grid, pan/zoom, minimap, fit-to-view).
   - **Palette**: drag or click-to-place each of the 8 node types at any point; nodes are placed at the drop position and `PATCH`ed to the engine (`position`) on drag end (debounced).
   - **Nodes**: distinct glyph + colour per type; agent nodes show live status (stopped/starting/needs_auth/running/idle/error) + harness badge (Claude/Codex); name editable inline (validated `^[a-z0-9][a-z0-9-_]{0,62}$`).
   - **Wires**: connect handles → pick type (`read`/`write`/`send`) in a small popover; **validate against the §3 matrix client-side** (hide/disable invalid options) and surface the engine's rejection if it disagrees. Wire styling per type (e.g. dashed for send, solid for read, double for write) with a legend. Injection wires (ctx→agent send) get a distinct look.
   - **Inspector (right panel)** per node type: agent (harness, model, system prompt editor, run_on_startup, ephemeral_context toggles, start/stop/restart/clear buttons, **Authenticate** flow: calls `auth/begin`, shows device/paste-code UI or API-key field, polls `auth`), ctx (markdown editor + preview), table (columns editor + rows viewer with pagination + read-only SQL box), endpoint (method/path/response_mode + a copyable public URL `https://api.wheel.dev/p/<id>/<path>` + "capability http is off" warning), script (language + Monaco editor + "run" with args and output), mcp (transport, command/args/url/env), vault (key list; values write-only with a "set" field that never reads back), chest (file browser: ls/upload/download/delete).
   - **Agent drawer (bottom)**: tabbed per running agent — live log stream (from `/engine/v1/events` WS + `log` backfill), a chat box that `POST`s `agents/:id/send`, message history with from-node labels.
   - **Realtime**: one WS connection per open board to `/v1/projects/:id/engine/v1/events` (through the API); update node state, messages, logs. Reconnect with backoff; show a connection indicator.
   - Keyboard: `Delete` removes selection (confirm for nodes with data), `Cmd+K` palette, `Esc` closes panels. Undo for position changes is nice-to-have.
4. **Auth**: Clerk `<SignIn/>`/`<SignUp/>` with email/password + Google + GitHub enabled; `/app/*` in middleware matcher.
5. **Quality**: `pnpm lint`, `pnpm typecheck`, `pnpm test` (vitest for the wire-matrix helper + api client), Playwright smoke in `qa/` is QA's — but make the app testable: stable `data-testid`s on palette items, nodes, inspector fields, buttons.

## Non-negotiables
- Never render or cache vault values; never put the Clerk token in URLs; never call the engine directly — always via the API.
- Board must stay usable with 200 nodes (virtualize the log; throttle WS-driven re-renders).
- Until API is up, run against a **mock API** (`web/mock/` — MSW or a tiny Node server) that implements §4/§5 shapes so you are never blocked. Swap by env var.
- Design: commit to a specific aesthetic (see `frontend-design`), consistent tokens, dark+light, real empty states, real loading states.

## Suggested plan shape
M1 (day 1): scaffold + Clerk + project list + board with agent & ctx nodes + wire creation w/ matrix + inspector for those two + agent drawer with chat/log over mock API, then real API.
M2: all 8 node types' inspectors, table viewer, chest browser, vault, auth flow UI, endpoint URL, script runner, landing page.
M3: polish, keyboard, perf with many nodes, error states, ADVERSARY/QA fixes.
