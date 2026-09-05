import { NODE_NAME_RE, type NodeType } from "@/lib/schema";

/** Names are addresses other agents send to, so the rule is strict and the message says why. */
export function validateNodeName(name: string, taken: string[] = []): string | null {
  if (!name) return "Every node needs a name — agents address each other by it.";
  if (name.length > 63) return "Names stop at 63 characters.";
  if (!NODE_NAME_RE.test(name)) {
    return "Use lowercase letters, digits, hyphen and underscore, starting with a letter or digit.";
  }
  if (taken.includes(name)) return `Another node is already called “${name}”.`;
  return null;
}

export function validateEndpointPath(path: string): string | null {
  if (!path.startsWith("/")) return "Paths start with a slash.";
  if (path.includes("..")) return "Paths can't contain “..”.";
  if (/\s/.test(path)) return "Paths can't contain spaces.";
  return null;
}

export function validateColumnName(name: string): string | null {
  if (!/^[a-z_][a-z0-9_]{0,62}$/.test(name)) {
    return "Column names use lowercase letters, digits and underscore.";
  }
  return null;
}

export function validateChestKey(key: string): string | null {
  if (!key) return "Give the file a name.";
  if (key.startsWith("/")) return "Keys are relative — drop the leading slash.";
  if (key.includes("..")) return "Keys can't contain “..”.";
  return null;
}

/** A default name that is free on this board: agent, agent-2, agent-3… */
export function suggestName(type: NodeType, taken: string[]): string {
  const base = type;
  if (!taken.includes(base)) return base;
  for (let i = 2; i < 500; i++) {
    const candidate = `${base}-${i}`;
    if (!taken.includes(candidate)) return candidate;
  }
  return `${base}-${Date.now()}`;
}
