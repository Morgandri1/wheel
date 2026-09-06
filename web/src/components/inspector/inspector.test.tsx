import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { Inspector } from "@/components/inspector";
import { PanelBoundary } from "@/components/inspector/panel-boundary";
import type { EngineApi } from "@/lib/api";
import type { NodeType, Project, WheelNode } from "@/lib/schema";

/**
 * Every inspector, against the shapes the ENGINE actually sends.
 *
 * The endpoint panel took the whole board down in production because `Board.project` was typed as
 * a full `Project` while the engine sends `{id}` alone, so `project.capabilities.http` threw. The
 * fixtures here are copied from a live `GET /v1/board` — `state: null`, `auth` present, no wires —
 * rather than invented, because inventing them is what let the bug through in the first place.
 */

const project: Project = {
  id: "p1",
  owner_id: "u1",
  name: "orbit",
  capabilities: { http: false },
  status: "running",
  created_at: "2026-09-06T04:21:24Z",
  updated_at: "2026-09-06T04:21:25Z",
  ingress_base_url: "https://api.example.test/p/p1",
};

/** Config exactly as the live engine returned it for each type. */
const CONFIGS: Record<NodeType, unknown> = {
  agent: { harness: "claude", system_prompt: "", run_on_startup: false, ephemeral_context: false },
  ctx: { markdown: "# hi" },
  table: { columns: [{ name: "claim", type: "text" }] },
  endpoint: { method: "POST", path: "/hook", response_mode: "ack", auth: { mode: "none" } },
  script: { language: "python", source: "print(1)" },
  mcp: { transport: "stdio", command: "echo" },
  vault: { keys: ["ANTHROPIC_API_KEY"] },
  chest: {},
  tool: { kind: "http", base_url: "https://example.test", operations: [], source: { format: "manual", raw: "", imported_at: "2026-09-06T04:21:24Z" } },
};

const NODE_TYPES = Object.keys(CONFIGS) as NodeType[];

function node(type: NodeType): WheelNode {
  return {
    id: `n-${type}`,
    name: `a-${type}`,
    type,
    position: { x: 0, y: 0 },
    wires: [],
    // The engine sends null for every non-agent type, and the UI must not assume an object.
    state: null,
    config: CONFIGS[type],
  } as WheelNode;
}

// Every method a panel might reach for. A missing one throws inside render, which the boundary
// would then catch — turning "the panel works" into "the boundary works" without saying so.
const api = {
  agent: () => ({
    authStatus: vi.fn().mockResolvedValue({ authenticated: false, mode: null }),
    authBegin: vi.fn(),
    authComplete: vi.fn(),
    start: vi.fn(), stop: vi.fn(), restart: vi.fn(), clear: vi.fn(),
    log: vi.fn().mockResolvedValue({ lines: [] }),
  }),
  table: () => ({
    rows: vi.fn().mockResolvedValue({ rows: [], total: 0 }),
    query: vi.fn().mockResolvedValue({ columns: [], rows: [] }),
  }),
  chest: () => ({
    ls: vi.fn().mockResolvedValue({ entries: [] }),
    get: vi.fn(),
    put: vi.fn(),
    remove: vi.fn(),
  }),
  tools: {
    preview: vi.fn().mockResolvedValue({ operations: [], format: "openapi" }),
    reimport: vi.fn().mockResolvedValue({ operations: [], added: [], removed: [], kept: [] }),
    ops: vi.fn().mockResolvedValue({ operations: [] }),
    call: vi.fn(),
  },
  patchNode: vi.fn(),
  deleteNode: vi.fn(),
  putSecret: vi.fn(),
} as unknown as EngineApi;

afterEach(cleanup);

function renderInspector(n: WheelNode, p: Project = project) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={client}>
      <Inspector node={n} nodes={[n]} project={p} api={api} projectId="p1" onChanged={() => {}} />
    </QueryClientProvider>,
  );
}

describe("every inspector renders the engine's real shapes", () => {
  it.each(NODE_TYPES)("renders the %s panel without crashing", (type) => {
    renderInspector(node(type));
    // The header proves the panel mounted rather than the boundary catching a throw.
    expect(screen.getByText(`a-${type}`)).toBeDefined();
    expect(screen.queryByTestId("inspector-panel-error")).toBeNull();
  });
});

describe("the endpoint panel, specifically", () => {
  // The production crash: the engine's board carries only {id}, so anything reading
  // capabilities.http off it threw and unmounted the board.
  it("survives a project with no capabilities at all", () => {
    const partial = { id: "p1" } as unknown as Project;
    renderInspector(node("endpoint"), partial);
    expect(screen.queryByTestId("inspector-panel-error")).toBeNull();
    // With capabilities absent we must claim the URL is NOT reachable, never that it is.
    expect(screen.getByTestId("endpoint-http-off")).toBeDefined();
  });

  it("says the URL is off when http is disabled", () => {
    renderInspector(node("endpoint"));
    expect(screen.getByTestId("endpoint-http-off")).toBeDefined();
  });
});

describe("PanelBoundary", () => {
  function Boom(): React.ReactNode {
    throw new Error("field went missing");
  }

  it("keeps a broken panel from taking the board down", () => {
    // React logs the caught error; silence it so a passing test does not read like a failing one.
    const spy = vi.spyOn(console, "error").mockImplementation(() => {});
    render(
      <PanelBoundary nodeName="hook">
        <Boom />
      </PanelBoundary>,
    );
    expect(screen.getByTestId("inspector-panel-error")).toBeDefined();
    expect(screen.getByText(/field went missing/)).toBeDefined();
    spy.mockRestore();
  });
});
