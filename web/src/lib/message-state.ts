/**
 * §3c #12: the operator must be able to see exactly when their message landed.
 *
 * The engine is the single writer to an agent's stdin and delivers strictly one message per turn,
 * with user messages ordered ahead of queued agent/endpoint/script messages. So "queued" alone is
 * not enough — the person needs to know whether theirs is the one going in next, or sitting behind
 * someone else's. This module derives that from the message rows we already have.
 */
import type { AgentStatus, Message, MessageSender, MessageState } from "@/lib/schema";

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
    .filter((m) => m.to === agentId && m.state === "queued" && !m.last_error)
    .sort((a, b) => {
      const lane = laneRank(a) - laneRank(b);
      if (lane !== 0) return lane;
      return a.created_at.localeCompare(b.created_at);
    });
}

function laneRank(m: Message): number {
  return m.from.kind === "user" ? 0 : 1;
}

/** How a sender is named in the message list. Node senders are addresses, so they read as such. */
export function senderLabel(from: MessageSender): string {
  return from.kind === "node" ? from.name : from.kind === "user" ? "you" : "engine";
}

/** The word after the name: the node type, or the sender kind when there is no node behind it. */
export function senderKind(from: MessageSender): string {
  return from.kind === "node" ? from.type : from.kind;
}

/** The message the engine will write to stdin next, or null when the queue is empty. */
export function nextForAgent(messages: readonly Message[], agentId: string): Message | null {
  return deliveryOrder(messages, agentId)[0] ?? null;
}

export function displayState(
  message: Message,
  messages: readonly Message[],
  agentId: string,
  /** The recipient's live status. Without it a stuck queue cannot say WHY it is stuck. */
  agentStatus?: AgentStatus,
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
  const ahead = deliveryOrder(messages, agentId).findIndex((m) => m.id === message.id);
  const isNext = nextForAgent(messages, agentId)?.id === message.id;

  // SDK: an agent that cannot authenticate goes to `needs_auth` and its queue is PRESERVED rather
  // than eaten — a message here is safe, not lost. But "goes in as soon as the current turn
  // finishes" would be a lie: there is no turn, and none is coming until someone saves a
  // credential. A wrong explanation sends people looking in the wrong place, so the blocker is
  // named instead.
  // Only when the caller actually told us the status. Treating "not told" as "stopped" would
  // invent a blocker we have no evidence for, which is the same class of lie in the other
  // direction.
  const blocked = agentStatus ? BLOCKED_BY_AGENT[agentStatus] : undefined;
  if (blocked) {
    return {
      state: "queued",
      label: "Queued",
      detail: ahead > 0 ? `${blocked} ${ahead} ahead of this one.` : blocked,
      tone: "pending",
    };
  }

  if (isNext) {
    return {
      state: "queued",
      label: "Queued (next)",
      detail: "Goes in as soon as the current turn finishes.",
      tone: "next",
    };
  }
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

/**
 * Why a queue cannot drain, by agent status. A status absent from this map is one where delivery
 * is either happening or about to, and the ordinary "queued (next)" wording is the truthful one.
 */
const BLOCKED_BY_AGENT: Partial<Record<AgentStatus, string>> = {
  needs_auth: "Held safely until this agent has credentials — nothing is lost.",
  stopped: "Held until the agent is started.",
  budget_exhausted: "Held until this agent's budget is raised or reset.",
  error: "Held until the agent recovers or is restarted.",
};
