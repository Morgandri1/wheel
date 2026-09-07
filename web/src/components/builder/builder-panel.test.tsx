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
  plan: { create_nodes: ["brief", "worker"], patch_nodes: [], create_wires: [wire] },
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
    expect(applier).toHaveBeenCalledWith(expect.anything(), true);
    expect(screen.getByTestId("builder-plan").textContent).toMatch(/create 2 nodes, add 1 wire/);
    expect(screen.getByTestId("builder-plan").textContent).toMatch(/Nothing has been created yet/);
    expect((screen.getByTestId("btn-builder-apply") as HTMLButtonElement).disabled).toBe(false);
  });

  it("applies for real only on the second click, and reports what landed", async () => {
    const applier = vi.fn(async (_b: unknown, dry: boolean) =>
      dry ? plan : ({ kind: "applied", report: { created_nodes: ["brief", "worker"], patched_nodes: [], created_wires: [wire], failures: [] } } as ApplyOutcome),
    );
    await propose(applier);
    fireEvent.click(screen.getByTestId("btn-builder-preview"));
    await waitFor(() => expect(screen.queryByTestId("builder-plan")).not.toBeNull());
    fireEvent.click(screen.getByTestId("btn-builder-apply"));
    await waitFor(() => expect(screen.queryByTestId("builder-applied")).not.toBeNull());
    expect(applier).toHaveBeenLastCalledWith(expect.anything(), false);
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
});
