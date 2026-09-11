// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import "server-only";

/**
 * What the same-origin proxy will pass to the API, decided without I/O so every refusal is a test.
 *
 * The target origin is fixed server configuration; nothing in a request chooses it. What a request
 * CAN influence — the path, two headers, the body — is narrowed here:
 *  - only `/v1/projects…`, the routes the board calls. `/v1/auth/*` is served by the session
 *    routes instead: a login answered through a generic proxy would hand the token to page script.
 *  - no dot segment, encoded slash or backslash, so no path leaves `/v1/projects`.
 *  - only `x-project-id` and `content-type` in. Cookies and any client `x-auth-token` never leave.
 */

export const PROXY_PREFIX = "/api/wheel";
export const PROJECT_ID = /^[A-Za-z0-9-]{1,64}$/;
const SEGMENT = /^[A-Za-z0-9._~!$&'()*+,;=:@%-]+$/;
const HEADER_VALUE = /^[\t\x20-\x7e]{1,256}$/;
const FORBIDDEN_DECODED = /[/\\\u0000-\u001f\u007f]/;
const RETURNED_HEADERS = [
  "content-type",
  "content-disposition",
  "cache-control",
  "retry-after",
  "etag",
  "last-modified",
  "x-wheel-mock",
];

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

/** The API path a request to this proxy may reach, or null. */
export function upstreamPath(pathname: string): string | null {
  if (!pathname.startsWith(`${PROXY_PREFIX}/`)) return null;
  const path = pathname.slice(PROXY_PREFIX.length);
  const segments = path.split("/").slice(1);
  if (segments[0] !== "v1" || segments[1] !== "projects") return null;
  if (!segments.every(safeSegment)) return null;
  // Events reach the browser through this app's relay; a ticket minted here is only a second door.
  if (segments.length === 4 && segments[3] === "ws-ticket") return null;
  return path;
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

/**
 * Response headers the browser may see. `content-length` and `content-encoding` stay behind on
 * purpose: fetch has already decoded the body, so passing them on would describe bytes that are
 * not the ones being sent. `set-cookie` stays behind because only this app sets cookies.
 */
export function returnedResponseHeaders(upstream: Headers): Headers {
  const out = new Headers();
  for (const name of RETURNED_HEADERS) {
    const value = upstream.get(name);
    if (value !== null) out.set(name, value);
  }
  return out;
}

export function declaredLength(headers: Headers): number {
  const n = Number(headers.get("content-length"));
  return Number.isFinite(n) && n > 0 ? n : 0;
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

/** The whole body, or null the moment it passes `limit` — never buffered past the cap first. */
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
