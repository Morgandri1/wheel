/**
 * §3c #12: the operator must be able to see exactly when their message landed.
 *
 * The engine is the single writer to an agent's stdin and delivers strictly one message per turn,
 * with user messages ordered ahead of queued agent/endpoint/script messages. So "queued" alone is
 * not enough — the person needs to know whether theirs is the one going in next, or sitting behind
 * someone else's. This module derives that from the message rows we already have.
 */
import type { Message, MessageState } from "@/lib/schema";

export interface MessageDisplayState {
  state: MessageState | "blocked";
  /** What to put in the pill. */
  label: string;
  /** Sentence for the tooltip / error line; empty when there is nothing to add. */
  detail: string;
  /** Drives the pill colour token. */
  tone: "pending" | "next" | "live" | "done" | "error";
}

/** Priority lane first (§3c #12), then oldest first — the order the engine will deliver in. */
export function deliveryOrder(messages: readonly Message[], agentId: string): Message[] {
  return messages
    .filter((m) => m.to_node === agentId && m.state === "queued" && !m.last_error)
    .sort((a, b) => {
      const lane = laneRank(a) - laneRank(b);
      if (lane !== 0) return lane;
      return a.created_at.localeCompare(b.created_at);
    });
}

function laneRank(m: Message): number {
  return m.from_type === "user" ? 0 : 1;
}

/** The message the engine will write to stdin next, or null when the queue is empty. */
export function nextForAgent(messages: readonly Message[], agentId: string): Message | null {
  return deliveryOrder(messages, agentId)[0] ?? null;
}

export function displayState(
  message: Message,
  messages: readonly Message[],
  agentId: string,
): MessageDisplayState {
  if (message.last_error) {
    return {
      state: "blocked",
      label: "Blocked",
      // §3c #11: nothing is ever silently clipped — say why it is stuck, keep the body intact.
      detail: message.last_error,
      tone: "error",
    };
  }
  if (message.state === "consumed") {
    return {
      state: "consumed",
      label: "Consumed",
      detail: "The agent finished the turn that contained this message.",
      tone: "done",
    };
  }
  if (message.state === "delivered") {
    return {
      state: "delivered",
      label: "Delivered",
      detail: "Written to the agent's stdin. Waiting for the turn to finish.",
      tone: "live",
    };
  }
  if (nextForAgent(messages, agentId)?.id === message.id) {
    return {
      state: "queued",
      label: "Queued (next)",
      detail: "Goes in as soon as the current turn finishes.",
      tone: "next",
    };
  }
  const ahead = deliveryOrder(messages, agentId).findIndex((m) => m.id === message.id);
  return {
    state: "queued",
    label: "Queued",
    detail:
      ahead > 0
        ? `${ahead} message${ahead === 1 ? "" : "s"} ahead of this one.`
        : "Waiting for the agent to start.",
    tone: "pending",
  };
}
