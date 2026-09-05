import { describe, expect, it } from "vitest";
import {
  deliveryOrder,
  displayState,
  nextForAgent,
  senderKind,
  senderLabel,
} from "@/lib/message-state";
import type { Message, MessageSender } from "@/lib/schema";

const AGENT = "agent-1";

const FROM_AGENT: MessageSender = { kind: "node", id: "n", name: "researcher", type: "agent" };
const FROM_USER: MessageSender = { kind: "user" };

function msg(over: Partial<Message> & { id: string }): Message {
  return {
    from: FROM_AGENT,
    to: AGENT,
    body: "hi",
    sha256: "0".repeat(64),
    bytes: 2,
    reply_to: null,
    state: "queued",
    created_at: "2026-09-05T00:00:00.000Z",
    delivered_at: null,
    consumed_at: null,
    last_error: null,
    ...over,
  };
}

describe("delivery order (§3c #12 priority lane)", () => {
  it("puts the user's message ahead of queued agent messages", () => {
    const messages = [
      msg({ id: "a1", created_at: "2026-09-05T00:00:01.000Z" }),
      msg({ id: "a2", created_at: "2026-09-05T00:00:02.000Z" }),
      msg({ id: "u1", from: FROM_USER, created_at: "2026-09-05T00:00:09.000Z" }),
      msg({ id: "a3", created_at: "2026-09-05T00:00:03.000Z" }),
    ];
    expect(deliveryOrder(messages, AGENT).map((m) => m.id)).toEqual(["u1", "a1", "a2", "a3"]);
    expect(nextForAgent(messages, AGENT)?.id).toBe("u1");
  });

  it("keeps two rapid user sends in the order they were sent", () => {
    const messages = [
      msg({ id: "u2", from: FROM_USER, created_at: "2026-09-05T00:00:02.000Z" }),
      msg({ id: "u1", from: FROM_USER, created_at: "2026-09-05T00:00:01.000Z" }),
    ];
    expect(deliveryOrder(messages, AGENT).map((m) => m.id)).toEqual(["u1", "u2"]);
  });

  it("ignores messages for other agents, and already-delivered ones", () => {
    const messages = [
      msg({ id: "other", to: "agent-2", from: FROM_USER }),
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
      msg({ id: "u1", from: FROM_USER }),
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
    const queued = msg({ id: "m", from: FROM_USER });
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

describe("how a sender is named", () => {
  it("names a node sender by its address, and says what kind of node it is", () => {
    expect(senderLabel(FROM_AGENT)).toBe("researcher");
    expect(senderKind(FROM_AGENT)).toBe("agent");
    const endpoint: MessageSender = { kind: "node", id: "e", name: "inbound", type: "endpoint" };
    expect(senderLabel(endpoint)).toBe("inbound");
    expect(senderKind(endpoint)).toBe("endpoint");
  });

  it("speaks to the operator in the second person about their own messages", () => {
    expect(senderLabel(FROM_USER)).toBe("you");
    expect(senderKind(FROM_USER)).toBe("user");
  });

  it("attributes engine-originated messages to the engine, not to a node", () => {
    const system: MessageSender = { kind: "system" };
    expect(senderLabel(system)).toBe("engine");
    expect(senderKind(system)).toBe("system");
  });
});

describe("naming a sender", () => {
  it("names a node by its address, since that is what people wire and message", () => {
    expect(senderLabel(FROM_AGENT)).toBe("researcher");
    expect(senderKind(FROM_AGENT)).toBe("agent");
  });

  it("distinguishes the operator from the engine itself", () => {
    expect(senderLabel(FROM_USER)).toBe("you");
    expect(senderKind(FROM_USER)).toBe("user");
    expect(senderLabel({ kind: "system" })).toBe("engine");
    expect(senderKind({ kind: "system" })).toBe("system");
  });
});

describe("a queue that cannot drain says why", () => {
  const queued = (id: string, from: "user" | "node" = "user"): Message => ({
    id,
    from: from === "user" ? { kind: "user" } : { kind: "node", name: "peer", type: "agent" },
    to: "agent-1",
    body: "hi",
    sha256: "x",
    bytes: 2,
    state: "queued",
    created_at: `2026-09-05T10:0${id}:00Z`,
    last_error: null,
  } as unknown as Message);

  it.each([
    ["needs_auth", /credentials/i],
    ["stopped", /started/i],
    ["budget_exhausted", /budget/i],
    ["error", /recovers/i],
  ] as const)("names %s as the blocker rather than promising delivery", (status, pattern) => {
    const messages = [queued("1")];
    const d = displayState(messages[0]!, messages, "agent-1", status);
    expect(d.state).toBe("queued");
    // Never "goes in as soon as the current turn finishes" — there is no turn coming.
    expect(d.detail).not.toMatch(/as soon as/i);
    expect(d.detail).toMatch(pattern);
  });

  it("still says a message is safe, because SDK stopped eating them", () => {
    const messages = [queued("1")];
    expect(displayState(messages[0]!, messages, "agent-1", "needs_auth").detail).toMatch(/nothing is lost/i);
  });

  it("counts how many are ahead even while blocked", () => {
    const messages = [queued("1"), queued("2")];
    const d = displayState(messages[1]!, messages, "agent-1", "needs_auth");
    expect(d.detail).toMatch(/1 ahead/);
  });

  it.each(["running", "idle", "starting", "parked"] as const)(
    "keeps the ordinary wording when the agent is %s",
    (status) => {
      const messages = [queued("1")];
      expect(displayState(messages[0]!, messages, "agent-1", status).label).toBe("Queued (next)");
    },
  );
});
