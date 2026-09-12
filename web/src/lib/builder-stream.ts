// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

/**
 * Reading the Workflow Builder's stream (`docs/PROTOCOL.md` §"Workflow Builder").
 *
 * Pure: bytes in, frames out. The transport is `builder-client.ts`; keeping the grammar here is
 * what makes every refusal and every malformed frame a test rather than something only a live
 * engine can show.
 *
 * Nothing here throws. A frame this app cannot read is ignored rather than allowed to take down
 * the panel mid-answer — the engine's own `done` frame is the authority on what was said, so a
 * dropped delta costs a repaint and nothing else.
 */

/** Why a run ended badly. `stream_broken` is this client's own: the connection died mid-answer. */
export type BuilderErrorCode = "needs_auth" | "builder_error" | "timeout" | "too_long" | "stream_broken";

export type BuilderFrame =
  | { kind: "delta"; text: string }
  | { kind: "done"; text: string; boards: number }
  | { kind: "error"; code: BuilderErrorCode; message: string };

const ERROR_CODES: BuilderErrorCode[] = ["needs_auth", "builder_error", "timeout", "too_long", "stream_broken"];

function asErrorCode(value: unknown): BuilderErrorCode {
  return ERROR_CODES.includes(value as BuilderErrorCode) ? (value as BuilderErrorCode) : "builder_error";
}

/**
 * Server-Sent Events, one frame at a time, across chunk boundaries.
 *
 * A chunk is whatever the network handed over: half a frame, three frames, or a split inside a
 * word. Anything not yet terminated by a blank line stays buffered until it is.
 */
export class BuilderStream {
  private buffer = "";

  feed(chunk: string): BuilderFrame[] {
    this.buffer += chunk;
    const frames: BuilderFrame[] = [];
    for (;;) {
      const end = this.buffer.indexOf("\n\n");
      if (end === -1) break;
      const block = this.buffer.slice(0, end);
      this.buffer = this.buffer.slice(end + 2);
      const frame = readBlock(block);
      if (frame) frames.push(frame);
    }
    return frames;
  }
}

function readBlock(block: string): BuilderFrame | null {
  let event = "";
  const data: string[] = [];
  for (const line of block.split("\n")) {
    // A comment line (": keepalive") is a heartbeat, not an event.
    if (line.startsWith(":")) continue;
    if (line.startsWith("event:")) event = line.slice(6).trim();
    else if (line.startsWith("data:")) data.push(line.slice(5).replace(/^ /, ""));
  }
  if (!event || data.length === 0) return null;

  let payload: unknown;
  try {
    payload = JSON.parse(data.join("\n"));
  } catch {
    return null;
  }
  const body = (payload ?? {}) as { text?: unknown; boards?: unknown; code?: unknown; message?: unknown };

  if (event === "delta") {
    return typeof body.text === "string" ? { kind: "delta", text: body.text } : null;
  }
  if (event === "done") {
    return {
      kind: "done",
      text: typeof body.text === "string" ? body.text : "",
      boards: typeof body.boards === "number" ? body.boards : 0,
    };
  }
  if (event === "error") {
    return {
      kind: "error",
      code: asErrorCode(body.code),
      message: typeof body.message === "string" ? body.message : "The builder stopped.",
    };
  }
  return null;
}

/** An agent or vault whose credential the builder could run on. */
export interface CredentialSource {
  id: string;
  name: string;
}

export interface BuilderSources {
  agents: CredentialSource[];
  vaults: CredentialSource[];
}

/** A refusal that arrived BEFORE the stream opened, so it has a status rather than a frame. */
export interface BuilderRefusal {
  status: number;
  code: string;
  message: string;
  /** Present on `needs_auth`: what the user could point the builder at instead. */
  sources?: BuilderSources;
}

const REFUSAL_MESSAGE: Record<number, string> = {
  400: "The builder could not read that request.",
  403: "This project does not allow that credential.",
  409: "The builder needs a credential before it can answer.",
  413: "This board is too large to send to the builder.",
  429: "The builder is already working on this project. Wait for it to finish.",
};

/**
 * The body of a non-streaming answer, read defensively: the server's own words win, and a body
 * that is missing or unreadable still produces something a person can act on.
 */
export function readRefusal(status: number, body: unknown): BuilderRefusal {
  const b = (body ?? {}) as { error?: { code?: unknown; message?: unknown }; sources?: unknown };
  const code = typeof b.error?.code === "string" ? b.error.code : `http_${status}`;
  const message =
    typeof b.error?.message === "string" && b.error.message.trim()
      ? b.error.message
      : (REFUSAL_MESSAGE[status] ?? "The builder could not answer.");
  const refusal: BuilderRefusal = { status, code, message };
  const sources = readSources(b.sources);
  if (sources) refusal.sources = sources;
  return refusal;
}

function readSources(value: unknown): BuilderSources | undefined {
  if (typeof value !== "object" || value === null) return undefined;
  const { agents, vaults } = value as { agents?: unknown; vaults?: unknown };
  return { agents: readList(agents), vaults: readList(vaults) };
}

function readList(value: unknown): CredentialSource[] {
  if (!Array.isArray(value)) return [];
  return value.flatMap((entry) => {
    const e = entry as { id?: unknown; name?: unknown };
    return typeof e?.id === "string" && typeof e?.name === "string" ? [{ id: e.id, name: e.name }] : [];
  });
}

/**
 * Whether the user has to do something about a credential before the builder can answer at all.
 * `needs_auth` arrives two ways — as a 409 before the run, and as an error frame when the stored
 * credential is rejected — and both put the same question to the user.
 */
export function isCredentialProblem(code: string): boolean {
  return code === "needs_auth" || code === "policy" || code === "ambiguous_credential";
}
