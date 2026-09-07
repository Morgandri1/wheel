import { describe, expect, it } from "vitest";
import { publicReach, reachSentence } from "./endpoint-reach";
import type { EndpointNode, WheelNode } from "@/lib/schema";

const agent = (id: string, name: string) =>
  ({ id, name, type: "agent", position: { x: 0, y: 0 }, wires: [], config: {} }) as unknown as WheelNode;
const script = (id: string, name: string) =>
  ({ id, name, type: "script", position: { x: 0, y: 0 }, wires: [], config: {} }) as unknown as WheelNode;
const table = (id: string, name: string) =>
  ({ id, name, type: "table", position: { x: 0, y: 0 }, wires: [], config: {} }) as unknown as WheelNode;

const endpoint = (wires: { to: string; type: string }[], auth?: unknown) =>
  ({
    id: "e1",
    name: "telegram",
    type: "endpoint",
    position: { x: 0, y: 0 },
    wires,
    config: { method: "POST", path: "/tg", response_mode: "ack", ...(auth ? { auth } : {}) },
  }) as unknown as EndpointNode;

describe("publicReach", () => {
  it("says nothing about an endpoint nobody consumes", () => {
    expect(publicReach(endpoint([]), [agent("a", "pm")])).toEqual([]);
  });

  it("names the agent once the two safe halves are wired together", () => {
    const reached = publicReach(endpoint([{ to: "a", type: "send" }]), [agent("a", "pm")]);
    expect(reached).toEqual([{ name: "pm", type: "agent" }]);
  });

  it("treats absent auth as none, because the engine does", () => {
    // The field is optional. Reading "not set" as safe is the silent composition itself.
    expect(publicReach(endpoint([{ to: "a", type: "send" }]), [agent("a", "pm")])).toHaveLength(1);
  });

  it("goes quiet once the endpoint is authenticated", () => {
    const auth = { mode: "bearer", vault_ref: "v/k" };
    expect(publicReach(endpoint([{ to: "a", type: "send" }], auth), [agent("a", "pm")])).toEqual([]);
  });

  it("counts a script, which the request executes", () => {
    expect(publicReach(endpoint([{ to: "s", type: "send" }]), [script("s", "ingest")])).toEqual([
      { name: "ingest", type: "script" },
    ]);
  });

  it("ignores a wire that is not a send, and a target that only stores", () => {
    const nodes = [table("t", "rows"), agent("a", "pm")];
    expect(publicReach(endpoint([{ to: "t", type: "write" }]), nodes)).toEqual([]);
  });
});

describe("reachSentence", () => {
  it("states the consequence rather than labelling it insecure", () => {
    const s = reachSentence([{ name: "pm", type: "agent" }]) ?? "";
    expect(s).toContain("anyone who knows its URL can send messages to pm");
    expect(s.toLowerCase()).not.toContain("insecure");
    expect(s.toLowerCase()).not.toContain("warning");
  });

  it("lists several targets readably", () => {
    const s = reachSentence([
      { name: "pm", type: "agent" },
      { name: "qa", type: "agent" },
    ]);
    expect(s).toContain("pm and qa");
  });

  it("says nothing when there is nothing to say", () => {
    expect(reachSentence([])).toBeNull();
  });
});
