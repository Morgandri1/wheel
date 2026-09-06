import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { EndpointPanel } from "@/components/inspector/endpoint-panel";
import type { EndpointNode, Project } from "@/lib/schema";
import type { EngineApi } from "@/lib/api";

const patch = vi.hoisted(() => vi.fn());
vi.mock("@/lib/api", async (orig) => ({
  ...(await orig<Record<string, unknown>>()),
  projects: { patch },
}));

afterEach(cleanup);
beforeEach(() => {
  patch.mockReset().mockResolvedValue({});
});

const project = (http: boolean): Project => ({
  id: "p1",
  owner_id: "u1",
  name: "orbit",
  capabilities: { http },
  status: "running",
  created_at: "2026-09-06T04:21:24Z",
  updated_at: "2026-09-06T04:21:25Z",
  ingress_base_url: "https://api.example.test/p/p1",
});

const node = {
  id: "n1",
  name: "tg",
  type: "endpoint",
  position: { x: 0, y: 0 },
  wires: [],
  config: { method: "POST", path: "/tg", response_mode: "ack", auth: { mode: "none" } },
  state: null,
} as unknown as EndpointNode;

const vaultNode = {
  id: "v1",
  name: "secrets",
  type: "vault",
  position: { x: 0, y: 0 },
  wires: [],
  config: { keys: ["api-token"] },
  state: null,
} as unknown as import("@/lib/schema").WheelNode;

function renderPanel(
  http: boolean,
  overrides: { node?: EndpointNode; nodes?: import("@/lib/schema").WheelNode[]; api?: EngineApi } = {},
) {
  const activeNode = overrides.node ?? node;
  const api = overrides.api ?? ({ patchNode: vi.fn() } as unknown as EngineApi);
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={client}>
      <EndpointPanel
        node={activeNode}
        nodes={overrides.nodes ?? [activeNode]}
        project={project(http)}
        api={api}
        onChanged={() => {}}
      />
    </QueryClientProvider>,
  );
}

describe("turning public HTTP on", () => {
  it("puts the switch in the notice that reports the problem", () => {
    renderPanel(false);
    const notice = screen.getByTestId("endpoint-http-off");
    expect(notice.querySelector('[data-testid="btn-endpoint-enable-http"]')).not.toBeNull();
  });

  it("enables it in one click instead of sending someone to the project list", async () => {
    renderPanel(false);
    fireEvent.click(screen.getByTestId("btn-endpoint-enable-http"));
    await waitFor(() => expect(patch).toHaveBeenCalledWith("p1", { capabilities: { http: true } }));
  });

  it("keeps the notice out of the way once it is on", () => {
    renderPanel(true);
    expect(screen.queryByTestId("endpoint-http-off")).toBeNull();
  });
});

/**
 * The operator hit a bare 404 on `/tg` and read it as a typo. It was not: endpoint ingress does
 * not exist engine-side yet. Everything here is about not letting the panel repeat that mistake.
 */
describe("testing the public URL", () => {
  it("shows the status and body verbatim rather than a summary of them", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(new Response("nope", { status: 404, statusText: "Not Found" })),
    );
    renderPanel(true);
    fireEvent.click(screen.getByTestId("btn-endpoint-test"));

    await waitFor(() => expect(screen.getByTestId("endpoint-probe-status").textContent).toBe("404"));
    expect(screen.getByTestId("endpoint-probe-body").textContent).toContain("nope");
    vi.unstubAllGlobals();
  });

  it("does not let a bare 404 read as a bad path", async () => {
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(new Response("", { status: 404 })));
    renderPanel(true);
    fireEvent.click(screen.getByTestId("btn-endpoint-test"));

    await waitFor(() => expect(screen.getByTestId("endpoint-probe-verdict")).toBeDefined());
    const verdict = screen.getByTestId("endpoint-probe-verdict").textContent ?? "";
    expect(verdict).toMatch(/not built yet/i);
    expect(verdict).toMatch(/does not mean your path is wrong/i);
    vi.unstubAllGlobals();
  });

  it("uses the engine's own words when the API sends ingress_unavailable", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(
        new Response('{"error":{"code":"ingress_unavailable","message":"no ingress"}}', { status: 501 }),
      ),
    );
    renderPanel(true);
    fireEvent.click(screen.getByTestId("btn-endpoint-test"));

    await waitFor(() =>
      expect(screen.getByTestId("endpoint-probe-verdict").textContent).toMatch(
        /does not serve endpoints yet/i,
      ),
    );
    vi.unstubAllGlobals();
  });

  it("reports a blocked read as unreadable, not as a dead endpoint", async () => {
    vi.stubGlobal("fetch", vi.fn().mockRejectedValue(new TypeError("Failed to fetch")));
    renderPanel(true);
    fireEvent.click(screen.getByTestId("btn-endpoint-test"));

    await waitFor(() => expect(screen.getByTestId("endpoint-probe-unreadable")).toBeDefined());
    expect(screen.getByTestId("endpoint-probe-unreadable").textContent).toMatch(
      /not evidence that the endpoint is down/i,
    );
    vi.unstubAllGlobals();
  });

  it("shows no reading at all until one has been taken", () => {
    renderPanel(true);
    expect(screen.queryByTestId("endpoint-probe")).toBeNull();
  });
});

/**
 * §3d/§3: bearer auth resolves from a vault the endpoint holds a `read` wire to. The picker
 * must not let anyone type an unreachable vault name — it only offers what is actually wired.
 */
describe("bearer auth", () => {
  it("disables the bearer option until a vault is wired", () => {
    renderPanel(true);
    const bearerOption = screen.getByRole("option", { name: "Bearer token" }) as HTMLOptionElement;
    expect(bearerOption.disabled).toBe(true);
  });

  it("lets bearer be picked once a vault is wired, and offers its keys", () => {
    const wired = { ...node, wires: [{ to: "v1", type: "read" }] } as unknown as EndpointNode;
    renderPanel(true, { node: wired, nodes: [wired, vaultNode] });
    const bearerOption = screen.getByRole("option", { name: "Bearer token" }) as HTMLOptionElement;
    expect(bearerOption.disabled).toBe(false);

    fireEvent.change(screen.getByTestId("inspector-endpoint-auth-mode"), {
      target: { value: "bearer" },
    });
    expect(screen.getByTestId("inspector-endpoint-auth-vault-ref")).toBeDefined();
  });

  it("blocks save until a vault key is entered", () => {
    const wired = { ...node, wires: [{ to: "v1", type: "read" }] } as unknown as EndpointNode;
    renderPanel(true, { node: wired, nodes: [wired, vaultNode] });
    fireEvent.change(screen.getByTestId("inspector-endpoint-auth-mode"), {
      target: { value: "bearer" },
    });
    expect((screen.getByTestId("btn-endpoint-save") as HTMLButtonElement).disabled).toBe(true);
  });

  it("saves the vault_ref alongside the rest of the config", async () => {
    const wired = { ...node, wires: [{ to: "v1", type: "read" }] } as unknown as EndpointNode;
    const patchNode = vi.fn().mockResolvedValue({});
    renderPanel(true, {
      node: wired,
      nodes: [wired, vaultNode],
      api: { patchNode } as unknown as EngineApi,
    });

    fireEvent.change(screen.getByTestId("inspector-endpoint-auth-mode"), {
      target: { value: "bearer" },
    });
    fireEvent.change(screen.getByTestId("inspector-endpoint-auth-vault-ref"), {
      target: { value: "secrets/api-token" },
    });
    fireEvent.click(screen.getByTestId("btn-endpoint-save"));

    await waitFor(() =>
      expect(patchNode).toHaveBeenCalledWith(
        "n1",
        expect.objectContaining({
          config: expect.objectContaining({
            auth: { mode: "bearer", vault_ref: "secrets/api-token" },
          }),
        }),
      ),
    );
  });

  it("warns when bearer is already configured but its vault got unwired", () => {
    const brokenAuth = {
      ...node,
      config: { ...node.config, auth: { mode: "bearer", vault_ref: "secrets/api-token" } },
    } as unknown as EndpointNode;
    renderPanel(true, { node: brokenAuth, nodes: [brokenAuth] });
    expect(screen.getByTestId("endpoint-auth-no-vault")).toBeDefined();
    // The field that would let someone fix it is not shown either — there is nothing to pick from.
    expect(screen.queryByTestId("inspector-endpoint-auth-vault-ref")).toBeNull();
  });

  it("going back to the saved mode leaves nothing dirty to save", () => {
    const wired = { ...node, wires: [{ to: "v1", type: "read" }] } as unknown as EndpointNode;
    renderPanel(true, { node: wired, nodes: [wired, vaultNode] });

    fireEvent.change(screen.getByTestId("inspector-endpoint-auth-mode"), {
      target: { value: "bearer" },
    });
    fireEvent.change(screen.getByTestId("inspector-endpoint-auth-vault-ref"), {
      target: { value: "secrets/api-token" },
    });
    fireEvent.change(screen.getByTestId("inspector-endpoint-auth-mode"), {
      target: { value: "none" },
    });

    expect((screen.getByTestId("btn-endpoint-save") as HTMLButtonElement).disabled).toBe(true);
  });
});
