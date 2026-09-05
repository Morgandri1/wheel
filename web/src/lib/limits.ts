/**
 * §3c row 6: limits are documented and enforced client-side with a clear error BEFORE sending,
 * so nobody discovers them by having a message silently fail somewhere downstream.
 *
 * Sizes are UTF-8 bytes, not characters — a 200 KiB message of emoji is not 200 K characters.
 */
export const LIMITS = {
  /** Message body — `wheel msg`, the drawer chat box, endpoint deliveries. */
  messageBytes: 256 * 1024,
  /** A ctx node's markdown, and a single table row's JSON. */
  valueBytes: 1024 * 1024,
  /** One chest blob. */
  blobBytes: 50 * 1024 * 1024,
} as const;

export type LimitKind = keyof typeof LIMITS;

const encoder = new TextEncoder();

export function byteLength(value: string): number {
  return encoder.encode(value).length;
}

export function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(n < 10 * 1024 ? 1 : 0)} KiB`;
  return `${(n / (1024 * 1024)).toFixed(1)} MiB`;
}

const NOUN: Record<LimitKind, string> = {
  messageBytes: "Messages",
  valueBytes: "Values",
  blobBytes: "Files",
};

/**
 * Returns null when the value fits, or the sentence to show the person when it doesn't.
 * The engine never truncates (§3c row 11), so refusing here is the only way to keep a person
 * from writing something that will sit queued with an error.
 */
export function checkLimit(kind: LimitKind, bytes: number): string | null {
  const max = LIMITS[kind];
  if (bytes <= max) return null;
  return `${NOUN[kind]} are capped at ${formatBytes(max)}. This one is ${formatBytes(bytes)} — trim ${formatBytes(bytes - max)}.`;
}

export function checkTextLimit(kind: Exclude<LimitKind, "blobBytes">, value: string): string | null {
  return checkLimit(kind, byteLength(value));
}

/** How full the field is, for the counter under a text area. Clamped to 1. */
export function limitFraction(kind: LimitKind, bytes: number): number {
  return Math.min(1, bytes / LIMITS[kind]);
}
