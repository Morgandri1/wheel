# Web plan — wheel.dev landing + board UI

Owner: **Web**. Worktree `/Users/metatron/wheel-wt/web`, branch `web/main`. Scope: `web/**` only
(plus this file). Contract of record: `docs/ARCHITECTURE.md` §3 (data model + wire matrix),
§4 (engine control plane), §5 (public API).

---

## 0. Environment (resolved, no blockers)

- Node **22.13.0** already present via nvm (`~/.nvm/versions/node/v22.13.0/bin`); pnpm **9.15.4**
  activated through corepack. No brew install needed.
- Cargo/Docker are not needed by Web. Docker daemon is currently down on this host — irrelevant to
  `web/`, but it means I cannot run the real API/engine locally yet. Hence the mock server (§3) is
  the primary development target until API says otherwise.
- Everything runs from the worktree: `cd /Users/metatron/wheel-wt/web/web && pnpm dev`.

---

## 1. Design direction — "Patchbay"

Committing to one aesthetic so the app and landing read as the same object.

**Where it comes from.** Wheel's subject matter is *wiring*: patch bays, modular-synth panels,
breadboards, schematic legends. The visual language is milled panels and colour-coded cable, not
SaaS cards. Depth comes from hairlines and value steps, never from drop shadows.

**Colour — 6 base tokens (dark ground is canonical; light is a real, tuned second theme):**

| token        | dark      | light     | role                                    |
|--------------|-----------|-----------|-----------------------------------------|
| `panel-0`    | `#161B19` | `#F1F2ED` | app/canvas ground (green-shifted graphite / bone — deliberately *not* cream) |
| `panel-1`    | `#1F2624` | `#FFFFFF` | node plates, inspector, drawer          |
| `rule`       | `#33403C` | `#C6CBC2` | 1px hairlines; the only structural device |
| `ink`        | `#E6EBE7` | `#171C1A` | primary text                            |
| `ink-dim`    | `#8FA09A` | `#5C6963` | secondary text, units, timestamps       |
| `live`       | `#4ED6A9` | `#0E8F68` | "running" — the single vivid state colour |

**Accents are the wire code, not decoration.** The three wire types get the three accents, and they
appear *only* on wires, wire legends, and the affordances that create them:
`read #63B3F5` (signal in) · `write #E9A23B` (mutation) · `send #C77DFF` (message).
Injection (`ctx → agent`, send) draws as a doubled hairline in `send` with a tick pattern — visually
distinct because it behaves differently (prepended, not delivered).
Error/`needs_auth` uses `#F2645A`; that is the only red in the system.

**Type.**
- **Archivo** — UI and headings. Archivo Expanded at display sizes: wide, engineered, reads like
  control-panel signage. One family, two widths, does the work of two typefaces.
- **JetBrains Mono** — *identifiers only*: node names (they literally match
  `^[a-z0-9][a-z0-9-_]{0,62}$` and are the address other agents send to), code editors, log output,
  SQL, ingress URLs. Never for generic small labels — that habit is the tell I am avoiding.
- Scale: 12 / 13 / 15 / 18 / 24 / 34 / 52. Body 15/1.55, measure ≤ 68ch.

**Shape and structure.** Radius 2px on plates, 3px on buttons, 0 on wires/rules. No box-shadows
anywhere except a single 1px inset highlight on node plates (milled-edge read). Sentence case
throughout; no all-caps eyebrows, no `→` glyphs appended to buttons, no `A · B · C` meta strings.

**Motion.** One orchestrated moment on the landing hero: a message packet travels
`ctx → researcher → writer → endpoint` along the wires, once, then rests. Everywhere else motion is
reactive only — panels opening, a status pill changing, a wire snapping to a handle.
`prefers-reduced-motion` kills the packet and leaves the board static.

**Landing layout concept.** The hero *is* a board — rendered with the same node components the app
uses, at rest, correct, and legible. The headline sits on a node-shaped plate wired into that board,
so the first thing anyone sees is the product's actual grammar rather than a picture of it.

```
┌───────────────────────────────────────────────────────────────┐
│  wheel                                       docs   sign in   │
├───────────────────────────────────────────────────────────────┤
│                                                               │
│   ┌──────────────────────────┐        ┌────────────┐          │
│   │ They say don't reinvent  │══send══▶│ ○ agent    │          │
│   │ the wheel.               │        │ researcher │──read──┐ │
│   │ Sometimes you have to.   │        └────────────┘        │ │
│   │                          │              ║               ▼ │
│   │ [ Open the board ]       │              ║          ┌───────┐
│   └──────────────────────────┘              ▼          │ table │
│        ▲                              ┌────────────┐   └───────┘
│   ┌────┴─────┐                        │ ○ agent    │            │
│   │ ctx      │                        │ writer     │            │
│   │ house-   │                        └────────────┘            │
│   │ style.md │                                                  │
│   └──────────┘                                                  │
├───────────────────────────────────────────────────────────────┤
│  legend:  ── read    ══ write    ‥‥ send    ⌇⌇ injection      │
├───────────────────────────────────────────────────────────────┤
│  Eight node types.   Wires are permissions.   Runs in the cloud│
│  (schematic legend row — three columns, hairline-separated,     │
│   left-aligned, NOT three rounded cards)                        │
└───────────────────────────────────────────────────────────────┘
```

Left-aligned throughout; the board illustration is the only centred mass.

**Board layout.** Palette rail left (48px icons + labels on hover), canvas centre, inspector right
(360px, resizable), agent drawer bottom (collapsible, remembers height). Every panel is a plate on
`panel-1` separated from the canvas by a single `rule` hairline. Connection indicator lives in the
top-right of the canvas, next to fit-to-view and zoom controls.

**Self-check against the brief.** First pass had the landing as headline-over-three-cards with a
gradient accent; discarded — that is the default for any dev-tool page. Second pass had a warm
cream/serif treatment; discarded as a known cliché. The board-as-hero and wire-colour-as-accent are
specific to Wheel: they only work because the product's core idea is a typed graph.

---

## 2. File layout

```
web/
  package.json  next.config.ts  tsconfig.json  tailwind.config.ts  vitest.config.ts
  .env.example  .env.local (gitignored)
  mock/
    server.ts               # tiny Node/undici HTTP + ws server implementing §4/§5
    fixtures.ts             # seeded project + board (ctx house-style -> agent researcher -> agent writer)
    state.ts                # in-memory board, wire-matrix enforcement, fake agent turn loop
  src/
    app/
      layout.tsx  globals.css
      page.tsx                        # landing
      sign-in/[[...sign-in]]/page.tsx
      sign-up/[[...sign-up]]/page.tsx
      app/page.tsx                    # project list
      app/[projectId]/page.tsx        # board
    middleware.ts                     # Clerk matcher on /app/*
    lib/
      api.ts            # fetch client: x-auth-token + x-project-id, typed routes, error envelope
      auth.ts           # token provider shim: clerk | mock (NEXT_PUBLIC_AUTH_MODE)
      events.ts         # WS client: backoff reconnect, throttled dispatch, heartbeat
      wire-matrix.ts    # §3 matrix as data + allowedWireTypes(from,to) + explain()
      node-meta.ts      # per-type glyph, colour, label, default config, testid
      validate.ts       # node-name regex, endpoint path rules, table column names
      schema/           # generated from docs/schema/*.json (hand-written until SDK ships)
    store/board.ts      # zustand: selection, inspector tab, drawer tabs, pending ops, viewport
    components/
      board/  (Canvas, NodePlate, node types x8, WireEdge, WireTypePopover, Palette, Legend, Minimap)
      inspector/ (Inspector + one panel per node type, AuthFlow)
      drawer/ (AgentDrawer, LogStream (virtualized), ChatBox, MessageList)
      ui/ (Button, Field, Toggle, Pill, Dialog, Toast, CopyField, Empty, Skeleton)
    styles/tokens.css
```

`pnpm gen:types` runs `json-schema-to-typescript` over `docs/schema/*.json` into `src/lib/schema/`.
Until SDK exports it, `src/lib/schema/index.ts` is hand-written to match §3 exactly and is marked
`// HAND-WRITTEN — replaced by pnpm gen:types when docs/schema lands`.

---

## 3. Mock API (`web/mock/`) — so Web is never blocked

A ~400-line standalone Node server (no MSW; a real server means QA's Playwright and my browser both
hit the same thing, and the WS is real). `NEXT_PUBLIC_API_URL=http://localhost:8787`,
`NEXT_PUBLIC_AUTH_MODE=mock`. `pnpm mock` runs it; `pnpm dev:mock` runs both.

Implements, with §5 shapes and status codes: projects CRUD + start/stop/restart, the
`/v1/projects/:id/engine/*` proxy surface (board, nodes, wires, agent lifecycle, send, log, auth
begin/complete/get, vault PUT, table rows/query, chest ls/blob), and `GET /engine/v1/events` over
WS. It enforces the §3 wire matrix server-side too, so I can prove the client-side matrix and the
server rejection path agree — and it deliberately rejects one combination the client also rejects,
plus lets me force a disagreement via `?chaos=wire` to exercise the "engine disagreed" surface.

Fake agent behaviour: `start` → `starting` → (`needs_auth` if not authenticated) → `running`;
messages produce streamed log lines and a reply message after ~1.5s; `ephemeral_context` shows a
context-clear line. Auth begin returns a `device_code` flow with a fake user code.

This mock is also the executable record of what I *assume* §4 returns. Any place SDK's PROTOCOL.md
disagrees, the mock changes, not the contract.

---

## 4. Milestones

**M1 — vertical slice (day 1).**
1. Scaffold Next 15 + TS strict + Tailwind + tokens + fonts; landing route stubbed.
2. `lib/wire-matrix.ts` + vitest suite (every cell of §3, both directions, deny-by-default) —
   written first, since it is the piece three other surfaces depend on.
3. Mock server + fixtures.
4. `lib/api.ts` + `lib/auth.ts` shim + vitest for header/error handling.
5. `/app` project list: status pills, new project, delete-with-confirm, teaching empty state.
6. `/app/[projectId]` board: xyflow canvas, dotted grid, minimap, fit-to-view, palette with all 8
   types (agent + ctx fully functional), node plates with live status + harness badge, inline rename
   with regex validation, debounced position PATCH.
7. Wire creation: drag handle → popover offering only matrix-legal types → POST; engine rejection
   surfaced inline on the wire. Legend.
8. Inspector: agent (harness, model, system prompt, `run_on_startup`, `ephemeral_context`,
   start/stop/restart/clear, Authenticate flow) and ctx (Monaco markdown + preview).
9. Agent drawer: WS-driven log + chat + message history with from-node labels.
10. Swap `NEXT_PUBLIC_API_URL` to the real API when API says it is up; fix the deltas.

**M2 — remaining six inspectors + landing.** table (columns editor, paginated rows, read-only SQL),
endpoint (method/path/response_mode, copyable `https://api.wheel.dev/p/<id>/<path>`, capability-off
warning), script (Monaco + run with args + output), mcp, vault (write-only set field), chest (file
browser with upload/download/delete). Then the landing page proper, including the animated board.

**M3 — hardening.** Keyboard (`Delete` with data-loss confirm, `Cmd+K`, `Esc`), 200-node perf pass
(memoised node plates, virtualized log, WS dispatch throttled to one commit per animation frame),
error/offline states, light theme audit, QA + ADVERSARY findings.

Testids land with each component, not retrofitted. Convention, so QA can rely on it without asking:
`palette-<type>`, `node-<name>`, `node-<name>-status`, `wire-<from>-<to>-<type>`,
`inspector-<type>-<field>`, `btn-<action>`, `drawer-tab-<name>`, `log-line`, `chat-input`,
`chat-send`, `conn-indicator`. I will publish this list to QA via PM once the first components land.

---

## 5. Risks

- **Contract drift.** I am coding against my reading of §4 before PROTOCOL.md exists. Mitigated by
  the mock being the written form of my assumptions (§6) — cheap to correct, one file.
- **Clerk credentials.** No instance/keys yet. Mitigated by `NEXT_PUBLIC_AUTH_MODE=mock`, which
  swaps the token provider only; every call site is unchanged when real Clerk lands.
- **xyflow at 200 nodes.** Custom node components re-render on every WS state tick if naive.
  Mitigated by per-node state subscription in zustand + `memo`, measured in M3 with a 200-node
  fixture in the mock.
- **Vault leakage.** Values must never enter React state, TanStack cache, or a log. Enforced by
  having no read path in `api.ts` at all — there is only `putVaultKey`, no getter to misuse.
- **Docker down on this host** blocks me from ever running the real stack locally. If it stays down
  I test against a deployed API or QA's environment; not on my critical path yet.

---

## 6. Open questions for PM (my recommendation in each case; proceeding on it unless told otherwise)

1. **Board node shape.** §4 says `GET /v1/board → { nodes: [Node+state] }`. I am reading that as
   `{ ...node, state: { status, session_id, last_activity, last_error } | null }` — nested, not
   flattened, and `null`/absent for non-agent types. *Recommend: nested.*
2. **WS event envelope.** I need exact shapes for `node.state | message | log | board.changed`.
   *Recommend* SDK adopt what my mock emits: every frame `{ type, project_id, ts, ...payload }`,
   with `log` carrying `{ node_id, cursor, stream: "stdout"|"stderr"|"system", line }` and `message`
   carrying the full row from the `messages` table plus `from_name`/`from_type`. Happy to change to
   whatever PROTOCOL.md says — I just need it to say something.
3. **Local dev base URL** for the real API — *recommend* `http://localhost:8080`, and that API
   enables CORS for `http://localhost:3000` with `x-auth-token`/`x-project-id` in
   `Access-Control-Allow-Headers`.
4. **Clerk instance.** Who creates it, and can I get the publishable key + a test user? Not blocking
   until we want real sign-in in the slice.
5. **Node rename.** Renaming a node changes its address and (for tables) the sqlite table name. Is
   `PATCH /v1/nodes/:id {name}` safe while an agent is running, or should the UI block rename on
   non-stopped agents? *Recommend: engine allows it, UI warns.*
6. **Ingress host in the endpoint inspector.** Hard-coding `https://api.wheel.dev` is wrong in local
   dev. *Recommend* API expose the public base in `GET /v1/projects/:id` as `ingress_base_url`; until
   then I derive it from `NEXT_PUBLIC_API_URL`.
