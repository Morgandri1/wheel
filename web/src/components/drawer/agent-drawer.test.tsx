// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { AgentDrawer } from "./agent-drawer";
import { useBoardStore } from "@/store/board";
import type { EngineApi } from "@/lib/api";
import type { Message, WheelNode } from "@/lib/schema";

afterEach(cleanup);

const AGENT = "a1";
const agentNode = {
  id: AGENT,
  name: "planner",
  type: "agent",
  position: { x: 0, y: 0 },
  wires: [],
  config: { harness: "claude", system_prompt: "", run_on_startup: false, ephemeral_context: false },
  state: { kind: "agent", status: "idle" },
} as unknown as WheelNode;

/** Shapes copied from sdk's finding-062 contract: redacted_streams on the log page, redacted: true on a message. */
const PLACEHOLDER = "[hidden: prompter tier or above]";
const message = (id: string, body: string, extra: object = {}): Message =>
  ({
    id,
    body,
    bytes: body.length,
    sha256: `sha-${id}-abcdef`,
    from: { kind: "user" },
    to: AGENT,
    state: "consumed",
    created_at: "2026-09-21T00:00:00Z",
    ...extra,
  }) as unknown as Message;

function renderDrawer(opts: { tier?: string; page: { lines: unknown[]; redacted_streams?: string[] }; messages?: Message[] }) {
  const log = vi.fn(async () => ({ next: 0, ...opts.page }));
  const api = { agent: () => ({ log, send: vi.fn() }) } as unknown as EngineApi;
  useBoardStore.getState().reset();
  useBoardStore.getState().openTab(AGENT);
  useBoardStore.getState().applyEvents((opts.messages ?? []).map((m) => ({ type: "message", message: m }) as never));
  render(<AgentDrawer nodes={[agentNode]} api={api} projectId="p1" tier={opts.tier} />);
  return { log };
}

const openTranscript = () => fireEvent.click(screen.getByTestId("drawer-view-transcript"));
const openMessages = () => fireEvent.click(screen.getByTestId("drawer-view-messages"));

beforeEach(() => useBoardStore.getState().reset());

describe("the transcript tab for a caller the engine withheld it from", () => {
  it("says it is hidden, from the log page's marker alone — never 'nothing written', which would be false", async () => {
    const { log } = renderDrawer({ page: { lines: [], redacted_streams: ["transcript"] } });
    await waitFor(() => expect(log).toHaveBeenCalled());
    openTranscript();
    await waitFor(() => expect(screen.queryByTestId("transcript-hidden")).not.toBeNull());
    expect(screen.getByTestId("transcript-hidden").textContent).toMatch(/Hidden — prompter tier or above/);
    expect(screen.queryByText(/Nothing written to this agent/)).toBeNull();
  });

  it("says it from a guest's tier alone, before (or without) a marker", async () => {
    renderDrawer({ tier: "guest", page: { lines: [] } });
    openTranscript();
    expect(screen.getByTestId("transcript-hidden")).toBeTruthy();
  });

  it("is an ordinary state, not an error: nothing toasts and no alert role appears", async () => {
    renderDrawer({ tier: "guest", page: { lines: [] } });
    openTranscript();
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("leaves the ordinary log alone: the other streams still render", async () => {
    renderDrawer({
      tier: "guest",
      page: { lines: [{ at: "t", node_id: AGENT, seq: 1, stream: "stdout", text: "working on it" }], redacted_streams: ["transcript"] },
    });
    await waitFor(() => expect(screen.queryByText(/working on it/)).not.toBeNull());
  });
});

describe("the transcript tab for a caller who may see it", () => {
  it.each(["prompter", "admin"])("shows the transcript for %s, and the honest empty state when it is empty", async (tier) => {
    const { log } = renderDrawer({ tier, page: { lines: [] } });
    await waitFor(() => expect(log).toHaveBeenCalled());
    openTranscript();
    expect(screen.queryByTestId("transcript-hidden")).toBeNull();
    expect(screen.getByText(/Nothing written to this agent/)).toBeTruthy();
  });

  it("does not call an unknown tier hidden: no tier is not evidence of a guest", async () => {
    const { log } = renderDrawer({ page: { lines: [] } });
    await waitFor(() => expect(log).toHaveBeenCalled());
    openTranscript();
    expect(screen.queryByTestId("transcript-hidden")).toBeNull();
  });
});

describe("the message list", () => {
  it("shows a redacted message as hidden, keeps its metadata, and never prints the placeholder body", () => {
    renderDrawer({
      tier: "guest",
      page: { lines: [] },
      messages: [message("m1", PLACEHOLDER, { redacted: true }), message("m2", "the plan is ready")],
    });
    openMessages();
    expect(screen.getByTestId("msg-m1-hidden").textContent).toBe("Hidden — prompter tier or above");
    expect(screen.queryByText(PLACEHOLDER)).toBeNull();
    expect(screen.getByTestId("msg-m1-sha").textContent).toBe("sha-m1-a");
    expect(screen.getByTestId("msg-m1-state")).toBeTruthy();
  });

  it("shows an unredacted message's body exactly as before, including a guest's own", () => {
    renderDrawer({ tier: "guest", page: { lines: [] }, messages: [message("m2", "the plan is ready")] });
    openMessages();
    expect(screen.getByText("the plan is ready")).toBeTruthy();
    expect(screen.queryByTestId("msg-m2-hidden")).toBeNull();
  });

  it("keys off the flag: a message that only quotes the placeholder text is shown as written", () => {
    renderDrawer({ tier: "prompter", page: { lines: [] }, messages: [message("m3", PLACEHOLDER)] });
    openMessages();
    expect(screen.getByText(PLACEHOLDER)).toBeTruthy();
    expect(screen.queryByTestId("msg-m3-hidden")).toBeNull();
  });
});
