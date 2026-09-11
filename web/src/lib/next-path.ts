// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

const FALLBACK = "/app";

/**
 * Where to go after sign-in: a path on this origin, or `/app`. An open redirect is exactly what a
 * sign-in page gets used for, and pattern-matching loses to spellings the browser normalizes —
 * `/\evil.com` is read as `//evil.com`, and tabs and newlines are stripped. So after a cheap check
 * for a single leading `/`, the value is parsed the way the browser will parse it and must land on
 * this origin.
 *
 * And so must what is RETURNED, parsed again: normalizing can itself make a protocol-relative path
 * (`/..//evil.com` resolves to the path `//evil.com`, which the browser reads as another host).
 */
export function safeNextPath(raw: string | null, origin: string): string {
  if (!raw || raw[0] !== "/" || raw[1] === "/" || raw[1] === "\\") return FALLBACK;
  const expected = new URL(origin).origin;
  let path: string;
  try {
    const url = new URL(raw, origin);
    if (url.origin !== expected) return FALLBACK;
    path = `${url.pathname}${url.search}${url.hash}`;
    if (new URL(path, origin).origin !== expected) return FALLBACK;
  } catch {
    return FALLBACK;
  }
  return path;
}
