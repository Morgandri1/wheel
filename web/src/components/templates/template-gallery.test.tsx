import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { TemplateGallery, type TemplateInstantiator, type TemplateLoader } from "./template-gallery";
import type { TemplateFile } from "@/lib/templates";
import type { TemplateSummary } from "@/app/api/templates/route";
import type { InstantiateOutcome } from "@/lib/templates";

afterEach(cleanup);

const summary = (overrides: Partial<TemplateSummary> = {}): TemplateSummary => ({
  slug: "research-crew",
  title: "Research crew",
  description: "An agent and its notes.",
  nodeCount: 2,
  wireCount: 1,
  warnings: [],
  requiresCapabilities: { http: false },
  ...overrides,
});

const file = (overrides: Partial<TemplateFile> = {}): TemplateFile => ({
  title: "Research crew",
  description: "An agent and its notes.",
  requiresCapabilities: { http: false },
  board: {
    nodes: [
      { name: "notes", type: "ctx", config: { markdown: "" }, position: { x: 0, y: 0 } },
      { name: "researcher", type: "agent", config: { harness: "claude", system_prompt: "p" }, position: { x: 0, y: 0 } },
    ],
    wires: [{ from: "notes", to: "researcher", type: "send" }],
  },
  ...overrides,
});

const loaderOf = (t: TemplateFile): TemplateLoader => async () => ({ status: "ok", template: t });

const created: InstantiateOutcome = {
  kind: "created",
  project: {
    id: "p1",
    owner_id: "u1",
    name: "Research crew",
    capabilities: { http: false },
    status: "starting",
    created_at: "2026-01-01T00:00:00Z",
    updated_at: "2026-01-01T00:00:00Z",
  },
  report: { created_nodes: ["notes", "researcher"], patched_nodes: [], created_wires: [], failures: [] },
};

async function openFirstCard(loadTemplate: TemplateLoader, instantiate: TemplateInstantiator) {
  render(
    <TemplateGallery summaries={[summary()]} loadTemplate={loadTemplate} instantiate={instantiate} />,
  );
  fireEvent.click(screen.getByTestId("template-card-research-crew"));
  await waitFor(() => expect(screen.queryByTestId("dialog-template-preview")).not.toBeNull());
}

describe("TemplateGallery — nothing exists until confirmed", () => {
  it("lists every summary as a card", () => {
    render(
      <TemplateGallery
        summaries={[summary(), summary({ slug: "webhook-logger", title: "Webhook logger" })]}
        loadTemplate={loaderOf(file())}
        instantiate={vi.fn()}
      />,
    );
    expect(screen.getByTestId("template-card-research-crew")).not.toBeNull();
    expect(screen.getByTestId("template-card-webhook-logger")).not.toBeNull();
  });

  it("shows an empty state rather than a bare blank page when there are no templates", () => {
    render(<TemplateGallery summaries={[]} loadTemplate={loaderOf(file())} instantiate={vi.fn()} />);
    expect(screen.getByText(/no templates yet/i)).not.toBeNull();
  });

  it("loads and previews the full board only once a card is opened", async () => {
    const loadTemplate = vi.fn(loaderOf(file()));
    await openFirstCard(loadTemplate, vi.fn());
    expect(loadTemplate).toHaveBeenCalledWith("research-crew");
    expect(screen.getByTestId("builder-preview").textContent).toMatch(/2 nodes, 1 wires/);
    expect(screen.getAllByTestId("builder-preview-node")).toHaveLength(2);
  });

  it("prefills the project name from the template's title", async () => {
    await openFirstCard(loaderOf(file()), vi.fn());
    expect((screen.getByTestId("input-template-project-name") as HTMLInputElement).value).toBe(
      "Research crew",
    );
  });

  it("disables Use until a name is present", async () => {
    await openFirstCard(loaderOf(file()), vi.fn());
    fireEvent.change(screen.getByTestId("input-template-project-name"), { target: { value: "" } });
    expect((screen.getByTestId("btn-use-template") as HTMLButtonElement).disabled).toBe(true);
  });

  it("creates the project on confirm and shows a link to it", async () => {
    const instantiate = vi.fn(async () => created);
    const onCreated = vi.fn();
    render(
      <TemplateGallery
        summaries={[summary()]}
        loadTemplate={loaderOf(file())}
        instantiate={instantiate}
        onCreated={onCreated}
      />,
    );
    fireEvent.click(screen.getByTestId("template-card-research-crew"));
    await waitFor(() => expect(screen.queryByTestId("dialog-template-preview")).not.toBeNull());
    fireEvent.click(screen.getByTestId("btn-use-template"));
    await waitFor(() => expect(screen.queryByTestId("template-outcome-created")).not.toBeNull());
    expect(instantiate).toHaveBeenCalledWith("Research crew", file().board, { http: false });
    expect(onCreated).toHaveBeenCalledWith(created.project);
    expect(screen.getByTestId("link-go-to-project").getAttribute("href")).toBe("/app/p1");
  });

  it("says the board is untouched when the server refuses it, without builder-specific wording", async () => {
    const refused: InstantiateOutcome = {
      kind: "refused",
      message: "the board was refused; nothing was created",
      refusals: [{ code: "wire_not_allowed", message: "no wire is allowed" }],
    };
    await openFirstCard(loaderOf(file()), vi.fn(async () => refused));
    fireEvent.click(screen.getByTestId("btn-use-template"));
    await waitFor(() => expect(screen.queryByTestId("template-outcome-refused")).not.toBeNull());
    const text = screen.getByTestId("template-outcome-refused").textContent ?? "";
    expect(text).toMatch(/no wire is allowed/);
    expect(text).not.toMatch(/ask the builder/i);
  });

  it("reports a clean rollback as safe to retry, never as half-built", async () => {
    const rolledBack: InstantiateOutcome = {
      kind: "rolled_back",
      cleanedUp: true,
      report: { created_nodes: ["notes"], patched_nodes: [], created_wires: [], failures: [{ step: "capabilities", error: "boom" }] },
    };
    await openFirstCard(loaderOf(file()), vi.fn(async () => rolledBack));
    fireEvent.click(screen.getByTestId("btn-use-template"));
    await waitFor(() => expect(screen.queryByTestId("template-outcome-rolled-back")).not.toBeNull());
    const text = screen.getByTestId("template-outcome-rolled-back").textContent ?? "";
    expect(text).toMatch(/safe to try again/i);
    expect(text).not.toMatch(/half-built/i);
    expect(screen.getByTestId("template-outcome-failure").textContent).toMatch(/capabilities: boom/);
  });

  it("shows the orphan project id only when the rollback itself failed", async () => {
    const failedRollback: InstantiateOutcome = {
      kind: "rolled_back",
      cleanedUp: false,
      projectId: "p9",
      report: { created_nodes: [], patched_nodes: [], created_wires: [], failures: [] },
    };
    await openFirstCard(loaderOf(file()), vi.fn(async () => failedRollback));
    fireEvent.click(screen.getByTestId("btn-use-template"));
    await waitFor(() => expect(screen.queryByTestId("template-outcome-rolled-back")).not.toBeNull());
    expect(screen.getByTestId("template-outcome-orphan-id").textContent).toMatch(/p9/);
  });

  it("shows a load error rather than an empty dialog when the template file cannot be read", async () => {
    const loadTemplate: TemplateLoader = async () => ({ status: "invalid", problems: ["bad json"] });
    await openFirstCard(loadTemplate, vi.fn());
    expect(screen.getByTestId("template-load-error").textContent).toMatch(/bad json/);
    expect(screen.queryByTestId("btn-use-template")).toBeNull();
  });

  it("resets state on close, so reopening a different template does not show stale data", async () => {
    const loadTemplate: TemplateLoader = async (slug) => ({
      status: "ok",
      template: file({ title: slug === "research-crew" ? "Research crew" : "Webhook logger" }),
    });
    render(
      <TemplateGallery
        summaries={[summary(), summary({ slug: "webhook-logger", title: "Webhook logger" })]}
        loadTemplate={loadTemplate}
        instantiate={vi.fn()}
      />,
    );
    fireEvent.click(screen.getByTestId("template-card-research-crew"));
    await waitFor(() => expect(screen.queryByTestId("dialog-template-preview")).not.toBeNull());
    fireEvent.keyDown(document, { key: "Escape" });
    await waitFor(() => expect(screen.queryByTestId("dialog-template-preview")).toBeNull());

    fireEvent.click(screen.getByTestId("template-card-webhook-logger"));
    await waitFor(() => expect(screen.queryByTestId("dialog-template-preview")).not.toBeNull());
    expect((screen.getByTestId("input-template-project-name") as HTMLInputElement).value).toBe(
      "Webhook logger",
    );
  });
});
