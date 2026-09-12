// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { BuilderPanel } from "./builder-panel";
import { END, START } from "@/lib/workflow-proposal";
import type { ApplyOutcome } from "@/lib/board-apply";

// RTL does not auto-clean here; without this the DOM accumulates across tests.
afterEach(cleanup);

const wire = { from: "brief", to: "worker", type: "send" as const };
const board = {
  nodes: [
    { id: "p1", name: "brief", type: "ctx", position: { x: 0, y: 0 }, config: { markdown: "" }, wires: [{ to: "p2", type: "send" }] },
    { id: "p2", name: "worker", type: "agent", position: { x: 0, y: 0 }, config: { harness: "claude", system_prompt: "p" }, wires: [] },
  ],
};
const reply = (obj: unknown, prose = "Here is a board.") => `${prose}\n${START}\n${JSON.stringify(obj)}\n${END}`;
const runnerOf = (text: string) => () => (async function* () { yield text; })();

const plan: ApplyOutcome = {
  kind: "plan",
  digest: "plan-digest",
  plan: { create_nodes: ["brief", "worker"], patch_nodes: [], create_wires: [wire], patch_details: [], delete_wires: [], delete_nodes: [] },
};

async function propose(applier: (b: unknown, dry: boolean) => Promise<ApplyOutcome>, text = reply(board)) {
  render(<BuilderPanel runner={runnerOf(text)} applyBoard={applier} />);
  fireEvent.change(screen.getByTestId("builder-input"), { target: { value: "a researcher" } });
  fireEvent.click(screen.getByTestId("btn-builder-send"));
  await waitFor(() => expect(screen.queryByTestId("builder-preview")).not.toBeNull());
}

describe("BuilderPanel — plan, confirm, then apply", () => {
  it("cannot apply until the server has been asked what it would do", async () => {
    /**
     * The client's own validation is a fast refusal, not the authority. What the user confirms must
     * be the SERVER's plan, or they are approving something other than what will happen.
     */
    const applier = vi.fn(async () => plan);
    await propose(applier);
    expect((screen.getByTestId("btn-builder-apply") as HTMLButtonElement).disabled).toBe(true);

    fireEvent.click(screen.getByTestId("btn-builder-preview"));
    await waitFor(() => expect(screen.queryByTestId("builder-plan")).not.toBeNull());
    expect(applier).toHaveBeenCalledWith(expect.anything(), true, expect.anything());
    expect(screen.getByTestId("builder-plan").textContent).toMatch(/create 2 nodes, add 1 wire/);
    expect(screen.getByTestId("builder-plan").textContent).toMatch(/Nothing has been created yet/);
    expect((screen.getByTestId("btn-builder-apply") as HTMLButtonElement).disabled).toBe(false);
  });

  it("applies for real only on the second click, and reports what landed", async () => {
    const applier = vi.fn(async (_b: unknown, dry: boolean) =>
      dry ? plan : ({ kind: "applied", report: { created_nodes: ["brief", "worker"], patched_nodes: [], created_wires: [wire], deleted_nodes: [], deleted_wires: [], failures: [] } } as ApplyOutcome),
    );
    await propose(applier);
    fireEvent.click(screen.getByTestId("btn-builder-preview"));
    await waitFor(() => expect(screen.queryByTestId("builder-plan")).not.toBeNull());
    fireEvent.click(screen.getByTestId("btn-builder-apply"));
    await waitFor(() => expect(screen.queryByTestId("builder-applied")).not.toBeNull());
    expect(applier).toHaveBeenLastCalledWith(expect.anything(), false, expect.anything());
  });

  it("renders a 207 as PARTLY applied, naming the failed wire, not as an error", async () => {
    /**
     * 207 is a 2xx. Swallowing it as success would leave the user with a half-built board they
     * believe is finished — the outcome the whole apply step exists to prevent.
     */
    const partial: ApplyOutcome = {
      kind: "partial",
      report: {
        created_nodes: ["brief", "worker"], patched_nodes: [], created_wires: [],
        deleted_nodes: [], deleted_wires: [],
        failures: [{ step: "create wire brief -> worker (send)", error: "engine returned 400", wire }],
      },
    };
    const applier = vi.fn(async (_b: unknown, dry: boolean) => (dry ? plan : partial));
    await propose(applier);
    fireEvent.click(screen.getByTestId("btn-builder-preview"));
    await waitFor(() => expect(screen.queryByTestId("builder-plan")).not.toBeNull());
    fireEvent.click(screen.getByTestId("btn-builder-apply"));
    await waitFor(() => expect(screen.queryByTestId("builder-partial")).not.toBeNull());
    const text = screen.getByTestId("builder-partial").textContent ?? "";
    expect(text).toMatch(/Partly applied/);
    expect(text).toMatch(/brief → worker \(send\)/);
    expect(text).toMatch(/half-built/);
  });

  it("says the board is untouched when the server refuses it", async () => {
    const refused: ApplyOutcome = {
      kind: "refused",
      message: "the board was refused; nothing was created",
      refusals: [{ code: "wire_not_allowed", message: "agent → agent as read is not allowed" }],
      consent: null,
    };
    const applier = vi.fn(async () => refused);
    await propose(applier);
    fireEvent.click(screen.getByTestId("btn-builder-preview"));
    await waitFor(() => expect(screen.queryByTestId("builder-refused")).not.toBeNull());
    const text = screen.getByTestId("builder-refused").textContent ?? "";
    expect(text).toMatch(/not allowed/);
    expect(text).toMatch(/untouched/);
  });

  it("keeps the raw JSON out of the transcript", async () => {
    await propose(vi.fn(async () => plan), reply(board, "Two nodes."));
    const turn = screen.getByTestId("builder-turn-builder").textContent ?? "";
    expect(turn).toBe("Two nodes.");
    expect(turn).not.toMatch(/START-WORKFLOW|"nodes"/);
  });

  it("says so plainly when the builder run does not exist yet", () => {
    render(<BuilderPanel runner={null} applyBoard={vi.fn(async () => plan)} />);
    expect(screen.getByTestId("builder-unavailable")).not.toBeNull();
    expect((screen.getByTestId("builder-input") as HTMLTextAreaElement).disabled).toBe(true);
  });

  /**
   * A consent refusal is the one refusal the user CAN answer. Granting must re-plan rather than
   * apply: what they confirm has to be the plan their consent actually produced.
   */
  it("offers the consent the server asked for, and granting re-checks instead of applying", async () => {
    const refused: ApplyOutcome = {
      kind: "refused",
      message: "the board was refused; nothing was created",
      refusals: [{ code: "delete_not_permitted", message: "removing the table \"results\" destroys what it holds" }],
      consent: {
        would_delete: [{ name: "results", type: "table", destroys: "its rows are dropped and cannot be recovered" }],
        grant: ["allow_delete"],
      },
    };
    const applier = vi.fn(async (_b: unknown, dry: boolean) => (dry ? refused : plan));
    await propose(applier);

    fireEvent.click(screen.getByTestId("btn-builder-preview"));
    await waitFor(() => expect(screen.queryByTestId("builder-consent")).not.toBeNull());
    const consent = screen.getByTestId("builder-consent").textContent ?? "";
    expect(consent).toMatch(/DELETE the table/);
    expect(consent).toMatch(/rows are dropped/);

    fireEvent.click(screen.getByTestId("btn-builder-grant"));
    await waitFor(() => expect(applier).toHaveBeenCalledTimes(2));
    // Still a dry run, carrying the grant.
    expect(applier).toHaveBeenLastCalledWith(expect.anything(), true, { grants: { allow_delete: true } });
  });

  it("shows what a removal destroys before it can be applied, and confirms the plan it showed", async () => {
    const destructive: ApplyOutcome = {
      kind: "plan",
      digest: "digest-abc",
      plan: {
        create_nodes: [],
        patch_nodes: [],
        create_wires: [],
        patch_details: [],
        delete_wires: [{ from: "brief", to: "worker", type: "send" }],
        delete_nodes: [{ name: "results", type: "table", wires: [] }],
      },
    };
    const applier = vi.fn(async (_b: unknown, dry: boolean) =>
      dry
        ? destructive
        : ({
            kind: "applied",
            report: {
              created_nodes: [],
              patched_nodes: [],
              created_wires: [],
              deleted_nodes: ["results"],
              deleted_wires: [{ from: "brief", to: "worker", type: "send" }],
              failures: [],
            },
          } as ApplyOutcome),
    );
    await propose(applier);
    fireEvent.click(screen.getByTestId("btn-builder-preview"));
    await waitFor(() => expect(screen.queryByTestId("builder-plan-removals")).not.toBeNull());

    const removals = screen.getByTestId("builder-plan-removals").textContent ?? "";
    expect(removals).toMatch(/Deletes table/);
    expect(removals).toMatch(/cannot be recovered/);
    expect(screen.getByTestId("builder-plan").textContent).toMatch(/DELETE 1 node/);

    fireEvent.click(screen.getByTestId("btn-builder-apply"));
    await waitFor(() => expect(screen.queryByTestId("builder-applied")).not.toBeNull());
    // The digest of the plan the user read goes back with the apply, so a board that moved
    // underneath is caught rather than silently re-planned.
    expect(applier).toHaveBeenLastCalledWith(expect.anything(), false, {
      grants: {},
      expectPlan: "digest-abc",
    });
    expect(screen.getByTestId("builder-removed").textContent).toMatch(/Removed: 1 node/);
  });

  it("treats a plan that went stale as something to re-read, not a failure", async () => {
    const stale: ApplyOutcome = {
      kind: "stale",
      digest: "new-digest",
      message: "the board changed since this plan was shown; nothing was applied",
      plan: {
        create_nodes: ["brief"],
        patch_nodes: [],
        create_wires: [],
        patch_details: [],
        delete_wires: [],
        delete_nodes: [],
      },
    };
    const applier = vi.fn(async (_b: unknown, dry: boolean) => (dry ? plan : stale));
    await propose(applier);
    fireEvent.click(screen.getByTestId("btn-builder-preview"));
    await waitFor(() => expect(screen.queryByTestId("builder-plan")).not.toBeNull());
    fireEvent.click(screen.getByTestId("btn-builder-apply"));

    await waitFor(() => expect(screen.queryByTestId("builder-stale")).not.toBeNull());
    const text = screen.getByTestId("builder-stale").textContent ?? "";
    expect(text).toMatch(/changed since/);
    expect(text).toMatch(/Nothing was applied/);
    expect(screen.queryByTestId("builder-applied")).toBeNull();
  });

  /**
   * Improve emits only what changes, and wires existing nodes by the id the builder was shown.
   * Without the board as context every such wire reads as pointing at nothing.
   */
  it("reads an improve proposal against the board it was given", async () => {
    const known = [{ id: "existing-1", name: "pm", type: "agent" as const }];
    const delta = {
      nodes: [
        {
          id: "new-1",
          name: "brief",
          type: "ctx",
          position: { x: 0, y: 0 },
          config: { markdown: "" },
          wires: [{ to: "existing-1", type: "send" }],
        },
      ],
    };
    render(
      <BuilderPanel runner={runnerOf(reply(delta))} applyBoard={vi.fn(async () => plan)} known={known} />,
    );
    fireEvent.change(screen.getByTestId("builder-input"), { target: { value: "add a brief" } });
    fireEvent.click(screen.getByTestId("btn-builder-send"));

    await waitFor(() => expect(screen.queryByTestId("builder-preview")).not.toBeNull());
    expect(screen.queryByTestId("builder-invalid")).toBeNull();
    const wires = screen.getAllByTestId("builder-preview-wire");
    expect(wires).toHaveLength(1);
    expect(wires[0]?.textContent).toMatch(/brief → pm/);
  });

  it("warns about a removal the builder proposed, and about what cannot run", async () => {
    const known = [{ id: "old-1", name: "old-notes", type: "ctx" as const }];
    const withRemoval = {
      nodes: [
        {
          id: "n1",
          name: "worker",
          type: "agent",
          position: { x: 0, y: 0 },
          config: { harness: "codex", system_prompt: "p" },
          wires: [],
        },
      ],
      remove: { nodes: ["old-notes"], wires: [] },
    };
    render(
      <BuilderPanel runner={runnerOf(reply(withRemoval))} applyBoard={vi.fn(async () => plan)} known={known} />,
    );
    fireEvent.change(screen.getByTestId("builder-input"), { target: { value: "swap them" } });
    fireEvent.click(screen.getByTestId("btn-builder-send"));

    await waitFor(() => expect(screen.queryByTestId("builder-preview")).not.toBeNull());
    const warnings = screen.getAllByTestId("builder-warning").map((w) => w.textContent ?? "");
    expect(warnings.some((w) => /would be REMOVED/.test(w))).toBe(true);
    // Verified against the engine: a codex node is refused at creation, not merely idle.
    expect(warnings.some((w) => /codex harness, which the engine refuses/.test(w))).toBe(true);
  });
});
