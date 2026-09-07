import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";

// RTL does not auto-clean here, so without this the DOM accumulates and getByTestId finds the
// previous test's panel as well as this one's.
afterEach(cleanup);
import { BuilderPanel } from "./builder-panel";
import { END, START } from "@/lib/workflow-proposal";
import type { ApplyApi } from "@/lib/workflow-apply";

const board = {
  nodes: [
    { id: "p1", name: "brief", type: "ctx", position: { x: 0, y: 0 }, config: { markdown: "" }, wires: [{ to: "p2", type: "send" }] },
    { id: "p2", name: "worker", type: "agent", position: { x: 0, y: 0 }, config: { harness: "claude", system_prompt: "p" }, wires: [] },
  ],
};

const reply = (obj: unknown, prose = "Here is a board.") => `${prose}\n${START}\n${JSON.stringify(obj)}\n${END}`;

const runnerOf = (text: string) => () =>
  (async function* () {
    yield text;
  })();

const api = (over: Partial<ApplyApi> = {}): ApplyApi => ({
  createNode: vi.fn(async (n) => ({ id: `real-${n.name}` }) as never),
  createWire: vi.fn(async () => ({})),
  ...over,
});

describe("BuilderPanel", () => {
  it("cannot apply before the builder has proposed anything", () => {
    render(<BuilderPanel runner={runnerOf("hello")} api={api()} />);
    expect((screen.getByTestId("btn-builder-apply") as HTMLButtonElement).disabled).toBe(true);
  });

  it("previews the proposed board and applies ONLY on an explicit click", async () => {
    /**
     * The review step: an LLM's output becomes real nodes and wires, so nothing may be created
     * without a human having seen it. This asserts the preview exists and that apply does nothing
     * until clicked.
     */
    const a = api();
    render(<BuilderPanel runner={runnerOf(reply(board))} api={a} />);
    fireEvent.change(screen.getByTestId("builder-input"), { target: { value: "a researcher" } });
    fireEvent.click(screen.getByTestId("btn-builder-send"));

    await waitFor(() => expect(screen.queryByTestId("builder-preview")).not.toBeNull());
    expect(screen.getAllByTestId("builder-preview-node")).toHaveLength(2);
    expect(screen.getAllByTestId("builder-preview-wire")).toHaveLength(1);
    expect(a.createNode).not.toHaveBeenCalled();

    fireEvent.click(screen.getByTestId("btn-builder-apply"));
    await waitFor(() => expect(screen.queryByTestId("builder-apply-result")).not.toBeNull());
    expect(a.createNode).toHaveBeenCalledTimes(2);
    expect(screen.getByTestId("builder-apply-result").textContent).toMatch(/Applied: 2 nodes, 1 wires/);
  });

  it("says PARTLY applied, never applied, when something failed", async () => {
    const a = api({ createWire: vi.fn(async () => { throw new Error("refused"); }) });
    render(<BuilderPanel runner={runnerOf(reply(board))} api={a} />);
    fireEvent.change(screen.getByTestId("builder-input"), { target: { value: "x" } });
    fireEvent.click(screen.getByTestId("btn-builder-send"));
    await waitFor(() => expect(screen.queryByTestId("builder-preview")).not.toBeNull());
    fireEvent.click(screen.getByTestId("btn-builder-apply"));
    await waitFor(() => expect(screen.queryByTestId("builder-apply-result")).not.toBeNull());
    const text = screen.getByTestId("builder-apply-result").textContent ?? "";
    expect(text).toMatch(/Partly applied/);
    expect(text).toMatch(/refused/);
  });

  it("shows the refusal, and no apply button, when the proposed board is illegal", async () => {
    const illegal = { nodes: [
      { id: "a", name: "one", type: "agent", position: { x: 0, y: 0 }, config: {}, wires: [{ to: "b", type: "read" }] },
      { id: "b", name: "two", type: "agent", position: { x: 0, y: 0 }, config: {}, wires: [] },
    ] };
    render(<BuilderPanel runner={runnerOf(reply(illegal))} api={api()} />);
    fireEvent.change(screen.getByTestId("builder-input"), { target: { value: "x" } });
    fireEvent.click(screen.getByTestId("btn-builder-send"));
    await waitFor(() => expect(screen.queryByTestId("builder-invalid")).not.toBeNull());
    expect(screen.getByTestId("builder-invalid").textContent).toMatch(/not an allowed wire/);
    expect((screen.getByTestId("btn-builder-apply") as HTMLButtonElement).disabled).toBe(true);
  });

  it("keeps the raw JSON out of the transcript", async () => {
    render(<BuilderPanel runner={runnerOf(reply(board, "Two nodes."))} api={api()} />);
    fireEvent.change(screen.getByTestId("builder-input"), { target: { value: "x" } });
    fireEvent.click(screen.getByTestId("btn-builder-send"));
    await waitFor(() => expect(screen.queryByTestId("builder-turn-builder")).not.toBeNull());
    const turn = screen.getByTestId("builder-turn-builder").textContent ?? "";
    expect(turn).toBe("Two nodes.");
    expect(turn).not.toMatch(/START-WORKFLOW|"nodes"/);
  });

  it("says so plainly when the builder run does not exist yet", () => {
    render(<BuilderPanel runner={null} api={api()} />);
    expect(screen.getByTestId("builder-unavailable")).not.toBeNull();
    expect((screen.getByTestId("builder-input") as HTMLTextAreaElement).disabled).toBe(true);
  });
});
