/**
 * The §3 wire semantics matrix, as data.
 *
 * Default DENY: anything not enumerated here is rejected. The engine and the API
 * enforce this independently — this copy exists so the UI can offer only legal
 * wire types instead of letting people discover the rules through error toasts.
 * When the engine disagrees with us, the engine is right and we surface its
 * rejection verbatim (see `components/board/WireTypePopover`).
 *
 * Source of truth: docs/ARCHITECTURE.md §3. Keep in lockstep.
 */
import type { NodeType, WireType } from "@/lib/schema";

export interface WireRule {
  from: NodeType;
  to: NodeType;
  type: WireType;
  /** Short imperative for the wire-type popover. */
  label: string;
  /**
   * Plain language from the source's point of view — `wheel connections` style.
   * `grants` is the same sentence; the popover and the inspector read it under that
   * name because there it answers "what does picking this give me?".
   */
  outgoing: string;
  /** Plain language from the target's point of view. */
  incoming: string;
  /** The `wheel` commands this wire unlocks, verbatim from §3. */
  commands: string[];
  /** §3: `write` implies `read` on table and chest. */
  implies?: WireType[];
  /** ctx → agent is prepended to the prompt, not delivered as a turn. Drawn differently. */
  injection?: boolean;
}

export const WIRE_MATRIX: readonly WireRule[] = [
  // ── agent ─────────────────────────────────────────────────────────────────
  {
    from: "agent",
    to: "agent",
    type: "send",
    label: "Send messages",
    outgoing: "you can prompt it",
    incoming: "it can prompt you",
    commands: ['wheel msg <agent> "..."'],
  },
  {
    from: "agent",
    to: "ctx",
    type: "read",
    label: "Read markdown",
    outgoing: "you can read its markdown",
    incoming: "it can read your markdown",
    commands: ["wheel read <ctx>"],
  },
  {
    from: "agent",
    to: "ctx",
    type: "write",
    label: "Replace markdown",
    outgoing: "you can replace its markdown",
    incoming: "it can replace your markdown",
    commands: ['wheel write <ctx> "..."', "wheel write <ctx> --file f.md"],
    implies: ["read"],
  },
  {
    from: "agent",
    to: "table",
    type: "read",
    label: "Read rows",
    outgoing: "you can read its rows",
    incoming: "it can read your rows",
    commands: ["wheel read <table>/<row>", "wheel ls <table>", 'wheel query <table> "SELECT ..."'],
  },
  {
    from: "agent",
    to: "table",
    type: "write",
    label: "Write rows",
    outgoing: "you can write its rows",
    incoming: "it can write your rows",
    commands: ["wheel write <table>/<row> '<json>'", "wheel rm <table>/<row>"],
    implies: ["read"],
  },
  {
    from: "agent",
    to: "vault",
    type: "read",
    label: "Read secrets",
    outgoing: "you can read its secrets",
    incoming: "it can read your secrets",
    commands: ["wheel secret get <vault>/<key>", "(also exported as env vars at spawn)"],
  },
  {
    from: "agent",
    to: "chest",
    type: "read",
    label: "Read files",
    outgoing: "you can read its files",
    incoming: "it can read your files",
    commands: ["wheel read <chest>/<path>", "wheel ls <chest> [prefix]"],
  },
  {
    from: "agent",
    to: "chest",
    type: "write",
    label: "Write files",
    outgoing: "you can write its files",
    incoming: "it can write your files",
    commands: ["wheel write <chest>/<path> --file f", "wheel rm <chest>/<path>"],
    implies: ["read"],
  },
  {
    from: "agent",
    to: "script",
    type: "read",
    label: "Run as a tool",
    outgoing: "you can run it",
    incoming: "it can run you",
    commands: ["wheel run <script> [args...]"],
  },
  {
    from: "agent",
    to: "mcp",
    type: "read",
    label: "Attach tools",
    outgoing: "its tools are attached to you at next start",
    incoming: "your tools are attached to it at its next start",
    commands: ["(attached to the harness config — no CLI call)"],
  },

  // ── ctx ───────────────────────────────────────────────────────────────────
  {
    from: "ctx",
    to: "agent",
    type: "send",
    label: "Inject into prompt",
    outgoing: "your markdown is injected into its prompt",
    incoming: "its markdown is injected into your prompt",
    commands: ["(prepended as `# Context: <name>` on start and after every context clear)"],
    injection: true,
  },

  // ── endpoint ──────────────────────────────────────────────────────────────
  {
    from: "endpoint",
    to: "agent",
    type: "send",
    label: "Deliver hits as messages",
    outgoing: "your HTTP hits are delivered to it as messages",
    incoming: "its HTTP hits arrive as messages",
    commands: ["(each hit arrives as `<AgentPrompt from=\"<endpoint>\" type=\"endpoint\">`)"],
  },
  {
    from: "endpoint",
    to: "table",
    type: "write",
    label: "Insert body as a row",
    outgoing: "your JSON body is inserted as a row",
    incoming: "its JSON body is inserted as a row",
    commands: ["(engine inserts the request body — no CLI call)"],
  },
  {
    from: "endpoint",
    to: "script",
    type: "send",
    label: "Invoke with the request",
    outgoing: "your requests invoke it",
    incoming: "its requests invoke you",
    commands: ['(with response_mode "script", the script\'s stdout is the HTTP response body)'],
  },

  // ── script ────────────────────────────────────────────────────────────────
  {
    from: "script",
    to: "agent",
    type: "send",
    label: "Send messages",
    outgoing: "you can prompt it",
    incoming: "it can prompt you",
    commands: ['wheel msg <agent> "..."'],
  },
  {
    from: "script",
    to: "ctx",
    type: "read",
    label: "Read markdown",
    outgoing: "you can read its markdown",
    incoming: "it can read your markdown",
    commands: ["wheel read <ctx>"],
  },
  {
    from: "script",
    to: "ctx",
    type: "write",
    label: "Replace markdown",
    outgoing: "you can replace its markdown",
    incoming: "it can replace your markdown",
    commands: ['wheel write <ctx> "..."'],
    implies: ["read"],
  },
  {
    from: "script",
    to: "table",
    type: "read",
    label: "Read rows",
    outgoing: "you can read its rows",
    incoming: "it can read your rows",
    commands: ["wheel read <table>/<row>", "wheel ls <table>", 'wheel query <table> "SELECT ..."'],
  },
  {
    from: "script",
    to: "table",
    type: "write",
    label: "Write rows",
    outgoing: "you can write its rows",
    incoming: "it can write your rows",
    commands: ["wheel write <table>/<row> '<json>'", "wheel rm <table>/<row>"],
    implies: ["read"],
  },
  {
    from: "script",
    to: "chest",
    type: "read",
    label: "Read files",
    outgoing: "you can read its files",
    incoming: "it can read your files",
    commands: ["wheel read <chest>/<path>", "wheel ls <chest> [prefix]"],
  },
  {
    from: "script",
    to: "chest",
    type: "write",
    label: "Write files",
    outgoing: "you can write its files",
    incoming: "it can write your files",
    commands: ["wheel write <chest>/<path> --file f", "wheel rm <chest>/<path>"],
    implies: ["read"],
  },
  {
    from: "script",
    to: "vault",
    type: "read",
    label: "Read secrets",
    outgoing: "you can read its secrets",
    incoming: "it can read your secrets",
    commands: ["wheel secret get <vault>/<key>"],
  },
] as const;

/**
 * `grants` is `outgoing` under the name the popover wants. Derived rather than
 * typed twice, so the two can never drift.
 */
export const WIRE_RULES: readonly (WireRule & { grants: string })[] = WIRE_MATRIX.map((rule) => ({
  ...rule,
  grants: rule.outgoing,
}));

const key = (from: NodeType, to: NodeType, type: WireType) => `${from}>${to}:${type}`;

const RULES_BY_KEY = new Map<string, WireRule & { grants: string }>(
  WIRE_RULES.map((rule) => [key(rule.from, rule.to, rule.type), rule]),
);

/** Every wire type legal from `from` to `to`, in read/write/send order. */
export function allowedWireTypes(from: NodeType, to: NodeType): WireType[] {
  return WIRE_MATRIX.filter((r) => r.from === from && r.to === to).map((r) => r.type);
}

export function allowedWireRules(
  from: NodeType,
  to: NodeType,
): (WireRule & { grants: string })[] {
  return WIRE_RULES.filter((r) => r.from === from && r.to === to);
}

export function isWireAllowed(from: NodeType, to: NodeType, type: WireType): boolean {
  return RULES_BY_KEY.has(key(from, to, type));
}

export function wireRule(
  from: NodeType,
  to: NodeType,
  type: WireType,
): (WireRule & { grants: string }) | undefined {
  return RULES_BY_KEY.get(key(from, to, type));
}

/** True if any wire at all may leave `from` for `to` — used to grey dead handles. */
export function canConnect(from: NodeType, to: NodeType): boolean {
  return allowedWireTypes(from, to).length > 0;
}

/** True if this type may originate any wire — ctx/table/vault/chest/mcp mostly cannot. */
export function hasOutgoingWires(from: NodeType): boolean {
  return WIRE_MATRIX.some((r) => r.from === from);
}

export function hasIncomingWires(to: NodeType): boolean {
  return WIRE_MATRIX.some((r) => r.to === to);
}

/** Node types `from` may legally wire to. Drives the palette's "what next" hints. */
export function connectableTargets(from: NodeType): NodeType[] {
  return [...new Set(WIRE_MATRIX.filter((r) => r.from === from).map((r) => r.to))];
}

/**
 * Why a wire was refused, in the engine's own voice (§3: exit code 3 wording).
 * Shown before we ever hit the network, so the refusal is instant.
 */
export function explainDenial(
  fromName: string,
  fromType: NodeType,
  toName: string,
  toType: NodeType,
  /** Omitted while dragging, before a wire type has been chosen. */
  type?: WireType,
): string {
  const legal = allowedWireTypes(fromType, toType);
  if (legal.length === 0) {
    return `No wire can go from ${fromType} to ${toType}. ${fromName} cannot reach ${toName}.`;
  }
  if (!type) return `${fromName} → ${toName} supports ${legal.join(", ")}.`;
  return `A ${fromType} cannot ${type} a ${toType}. Legal here: ${legal.join(", ")}.`;
}

/** ctx → agent (send) is prepended to the prompt rather than delivered — drawn as an injection. */
export function isInjection(from: NodeType, to: NodeType, type: WireType): boolean {
  return wireRule(from, to, type)?.injection === true;
}

/**
 * §3: on ctx, table and chest a `write` wire carries `read` with it, so the popover
 * can say that choosing write is not the narrower grant it looks like.
 */
export function impliesRead(from: NodeType, to: NodeType): boolean {
  return wireRule(from, to, "write")?.implies?.includes("read") ?? false;
}
