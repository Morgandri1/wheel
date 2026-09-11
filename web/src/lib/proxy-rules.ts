// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import "server-only";

/**
 * What the same-origin proxy passes to the API, decided without I/O so every refusal is a test.
 * Each refusal and its reason is listed once, in web/DEPLOY.md ("What the proxy refuses").
 */

export const PROXY_PREFIX = "/api/wheel";
export const PROJECT_ID = /^[A-Za-z0-9-]{1,64}$/;
const SEGMENT = /^[A-Za-z0-9._~!$&'()*+,=:@%-]+$/;
const HEADER_VALUE = /^[\t\x20-\x7e]{1,256}$/;
const FORBIDDEN_DECODED = /[%/\\\u0000-\u001f\u007f]/;
const PROJECT_ACTIONS = new Set(["start", "stop", "restart"]);
const RETURNED_HEADERS = [
  "content-type",
  "content-disposition",
  "cache-control",
  "retry-after",
  "etag",
  "last-modified",
  "x-wheel-mock",
];

/**
 * One path segment, decoded exactly once. A `%` that survives that decode is a second layer of
 * encoding aimed at a hop that decodes again (`%252e%252e` → `%2e%2e` → `..`), so it is refused
 * rather than decoded further — as are `.`, `..`, slashes, backslashes and control characters.
 */
export function safeSegment(raw: string): boolean {
  if (!SEGMENT.test(raw)) return false;
  let decoded: string;
  try {
    decoded = decodeURIComponent(raw);
  } catch {
    return false;
  }
  return decoded !== "." && decoded !== ".." && !FORBIDDEN_DECODED.test(decoded);
}

/**
 * The board's routes and nothing else, compared on the raw segments. A positive list: another
 * spelling of some route that is not on it — `ws%2Dticket`, `WS-TICKET` — is simply not on it.
 */
function isBoardRoute(segments: string[]): boolean {
  const [v1, projects, id, action, ...rest] = segments;
  if (v1 !== "v1" || projects !== "projects") return false;
  if (id === undefined || action === undefined) return true;
  if (PROJECT_ACTIONS.has(action)) return rest.length === 0;
  if (action === "board") return rest.length === 1 && rest[0] === "apply";
  if (action === "engine") return rest[0] === "v1" && rest.length >= 2;
  return false;
}

/** The API path a request to this proxy may reach, or null. */
export function upstreamPath(pathname: string): string | null {
  if (!pathname.startsWith(`${PROXY_PREFIX}/`)) return null;
  const path = pathname.slice(PROXY_PREFIX.length);
  const segments = path.split("/").slice(1);
  return segments.every(safeSegment) && isBoardRoute(segments) ? path : null;
}

/** `path` on the fixed API origin, or null if parsing would land it anywhere else. */
export function apiTarget(base: string, path: string, search = ""): string | null {
  const root = new URL(base);
  const prefix = root.pathname.replace(/\/+$/, "");
  const target = new URL(`${root.origin}${prefix}${path}${search}`);
  if (target.origin !== root.origin || target.pathname !== `${prefix}${path}`) return null;
  return target.toString();
}

/** A fresh header set holding only what the API needs, or null if a value is not one to pass on. */
export function forwardedRequestHeaders(incoming: Headers): Headers | null {
  const out = new Headers();
  const projectId = incoming.get("x-project-id");
  if (projectId !== null) {
    if (!PROJECT_ID.test(projectId)) return null;
    out.set("x-project-id", projectId);
  }
  const contentType = incoming.get("content-type");
  if (contentType !== null) {
    if (!HEADER_VALUE.test(contentType)) return null;
    out.set("content-type", contentType);
  }
  return out;
}

export function isJsonMediaType(type: string | null): boolean {
  const media = type?.split(";")[0]?.trim().toLowerCase() ?? "";
  return media === "application/json" || media.endsWith("+json");
}

/**
 * Response headers the browser may see, plus what makes an upstream body safe on this origin.
 * `content-length` and `content-encoding` stay behind (fetch already decoded the body), and so
 * does `set-cookie` (only this app sets cookies). A chest can hold anything, and HTML rendered on
 * this origin would run with the user's session — so nothing is sniffed, and anything that is not
 * JSON is sandboxed and downloaded rather than rendered.
 */
export function returnedResponseHeaders(upstream: Headers): Headers {
  const out = new Headers();
  for (const name of RETURNED_HEADERS) {
    const value = upstream.get(name);
    if (value !== null) out.set(name, value);
  }
  out.set("x-content-type-options", "nosniff");
  if (!isJsonMediaType(out.get("content-type"))) {
    out.set("content-security-policy", "sandbox");
    if (!out.get("content-disposition")?.toLowerCase().startsWith("attachment")) out.set("content-disposition", "attachment");
  }
  return out;
}

export function declaredLength(headers: Headers): number {
  const n = Number(headers.get("content-length"));
  return Number.isFinite(n) && n > 0 ? n : 0;
}

/** Passes a body through while counting it, and errors the stream the moment it passes `limit`. */
export function cappedStream(limit: number): { transform: TransformStream<Uint8Array, Uint8Array>; exceeded: () => boolean } {
  let total = 0;
  let over = false;
  const transform = new TransformStream<Uint8Array, Uint8Array>({
    transform(chunk, controller) {
      total += chunk.byteLength;
      if (total > limit) {
        over = true;
        controller.error(new RangeError(`request body over ${limit} bytes`));
        return;
      }
      controller.enqueue(chunk);
    },
  });
  return { transform, exceeded: () => over };
}

/** At most `limit` bytes, counted while streaming, and whether there was more. */
export async function readUpTo(
  body: ReadableStream<Uint8Array> | null,
  limit: number,
): Promise<{ bytes: Uint8Array<ArrayBuffer>; truncated: boolean }> {
  const chunks: Uint8Array[] = [];
  let total = 0;
  let truncated = false;
  if (body) {
    const reader = body.getReader();
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      const room = limit - total;
      if (value.byteLength > room) {
        chunks.push(value.subarray(0, room));
        total = limit;
        truncated = true;
        await reader.cancel().catch(() => {});
        break;
      }
      chunks.push(value);
      total += value.byteLength;
    }
  }
  const bytes = new Uint8Array(total);
  let offset = 0;
  for (const chunk of chunks) {
    bytes.set(chunk, offset);
    offset += chunk.byteLength;
  }
  return { bytes, truncated };
}

/** The whole body, or null the moment it passes `limit` — for the small bodies this app reads itself. */
export async function readCapped(
  body: ReadableStream<Uint8Array> | null,
  limit: number,
): Promise<Uint8Array<ArrayBuffer> | null> {
  const { bytes, truncated } = await readUpTo(body, limit);
  return truncated ? null : bytes;
}

/** The API's own error shape, so the client reads this app's refusals exactly as it reads the API's. */
export function errorEnvelope(status: number, code: string, message: string, headers?: HeadersInit): Response {
  return Response.json({ error: { code, message } }, { status, headers });
}
