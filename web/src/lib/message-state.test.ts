import { describe, expect, it } from "vitest";
import { deliveryOrder, displayState, nextForAgent } from "@/lib/message-state";
import type { Message } from "@/lib/schema";

const AGENT = "agent-1";

function msg(over: Partial<Message> & { id: string }): Message {
  return {
    from_node: "n",
    to_node: AGENT,
    body: "hi",
    sha256: "0".repeat(64),
    bytes: 2,
    reply_to: null,
    state: "queued",
    created_at: "2026-09-05T00:00:00.000Z",
    delivered_at: null,
    consumed_at: null,
    last_error: null,
    from_type: "agent",
    ...over,
  };
}

describe("delivery order (§3c #12 priority lane)", () => {
  it("puts the user's message ahead of queued agent messages", () => {
    const messages = [
      msg({ id: "a1", created_at: "2026-09-05T00:00:01.000Z" }),
      msg({ id: "a2", created_at: "2026-09-05T00:00:02.000Z" }),
      msg({ id: "u1", from_type: "user", created_at: "2026-09-05T00:00:09.000Z" }),
      msg({ id: "a3", created_at: "2026-09-05T00:00:03.000Z" }),
    ];
    expect(deliveryOrder(messages, AGENT).map((m) => m.id)).toEqual(["u1", "a1", "a2", "a3"]);
    expect(nextForAgent(messages, AGENT)?.id).toBe("u1");
  });

  it("keeps two rapid user sends in the order they were sent", () => {
    const messages = [
      msg({ id: "u2", from_type: "user", created_at: "2026-09-05T00:00:02.000Z" }),
      msg({ id: "u1", from_type: "user", created_at: "2026-09-05T00:00:01.000Z" }),
    ];
    expect(deliveryOrder(messages, AGENT).map((m) => m.id)).toEqual(["u1", "u2"]);
  });

  it("ignores messages for other agents, and already-delivered ones", () => {
    const messages = [
      msg({ id: "other", to_node: "agent-2", from_type: "user" }),
      msg({ id: "done", state: "consumed" }),
      msg({ id: "sent", state: "delivered" }),
      msg({ id: "wait" }),
    ];
    expect(deliveryOrder(messages, AGENT).map((m) => m.id)).toEqual(["wait"]);
  });

  it("does not schedule a blocked message", () => {
    const messages = [msg({ id: "stuck", last_error: "harness limit exceeded" })];
    expect(nextForAgent(messages, AGENT)).toBeNull();
  });
});

describe("display state", () => {
  it("labels the head of the queue as next", () => {
    const messages = [
      msg({ id: "u1", from_type: "user" }),
      msg({ id: "a1", created_at: "2026-09-05T00:00:01.000Z" }),
    ];
    expect(displayState(messages[0]!, messages, AGENT).label).toBe("Queued (next)");
    expect(displayState(messages[1]!, messages, AGENT).label).toBe("Queued");
  });

  it("says how many messages are ahead", () => {
    const messages = [
      msg({ id: "a1", created_at: "2026-09-05T00:00:01.000Z" }),
      msg({ id: "a2", created_at: "2026-09-05T00:00:02.000Z" }),
      msg({ id: "a3", created_at: "2026-09-05T00:00:03.000Z" }),
    ];
    expect(displayState(messages[2]!, messages, AGENT).detail).toBe("2 messages ahead of this one.");
    expect(displayState(messages[1]!, messages, AGENT).detail).toBe("1 message ahead of this one.");
  });

  it("walks queued → delivered → consumed", () => {
    const queued = msg({ id: "m", from_type: "user" });
    expect(displayState(queued, [queued], AGENT).state).toBe("queued");

    const delivered = msg({ id: "m", state: "delivered", delivered_at: "x" });
    expect(displayState(delivered, [delivered], AGENT).label).toBe("Delivered");

    const consumed = msg({ id: "m", state: "consumed", consumed_at: "x" });
    expect(displayState(consumed, [consumed], AGENT).label).toBe("Consumed");
  });

  it("surfaces a block instead of pretending the message is fine (§3c #11)", () => {
    const stuck = msg({ id: "m", last_error: "would exceed the harness context limit" });
    const d = displayState(stuck, [stuck], AGENT);
    expect(d.state).toBe("blocked");
    expect(d.tone).toBe("error");
    expect(d.detail).toContain("harness context limit");
  });
});
