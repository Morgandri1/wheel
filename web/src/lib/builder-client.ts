"use client";

// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

/**
 * Talking to the Workflow Builder, through this app's own server and nothing else.
 *
 * `/api/wheel/projects/:id/builder` is a relay on this origin (`src/lib/builder-relay.ts`) that
 * attaches the session server-side and streams the engine's Server-Sent Events back. The browser
 * holds no token and never learns where the API is — the same boundary every other call crosses.
 *
 * The builder's credential lives on the engine, so it is read and written through the ordinary
 * engine proxy path rather than a second relay.
 */
import { projectPath } from "@/lib/api-paths";
import { ApiError, notifyUnauthorized } from "@/lib/auth";
import {
  BuilderStream,
  readRefusal,
  type BuilderFrame,
  type BuilderRefusal,
} from "@/lib/builder-stream";

const PROXY = "/api/wheel";

export type BuilderMode = "new" | "improve";

/** Which of the project's own credentials a turn runs on. */
export type BuilderCredential =
  | { source: "builder" }
  | { source: "agent"; node: string }
  | { source: "vault"; node: string };

export interface BuilderTurn {
  role: "user" | "builder";
  text: string;
}

export interface BuilderRequest {
  mode: BuilderMode;
  turns: BuilderTurn[];
  credential?: BuilderCredential;
}

/** A run that never started, or stopped early. Carries the refusal so the UI can act on it. */
export class BuilderStopped extends Error {
  constructor(readonly refusal: BuilderRefusal) {
    super(refusal.message);
    this.name = "BuilderStopped";
  }
}

function builderPath(projectId: string): string {
  return `${PROXY}/projects/${encodeURIComponent(projectId)}/builder`;
}

/**
 * One turn, as frames. The caller sees text as it arrives and one terminal frame.
 *
 * A refusal that arrives before the stream — no credential, policy, busy — is thrown as
 * `BuilderStopped`, because there is nothing to render incrementally and the user has to answer
 * it. Anything that goes wrong once the stream is open arrives as an `error` frame instead: by
 * then the status line is long gone.
 */
export async function* builderTurns(
  projectId: string,
  request: BuilderRequest,
  signal?: AbortSignal,
): AsyncGenerator<BuilderFrame> {
  let res: Response;
  try {
    res = await fetch(builderPath(projectId), {
      method: "POST",
      headers: { "content-type": "application/json" },
      credentials: "same-origin",
      body: JSON.stringify(request),
      signal,
    });
  } catch (cause) {
    if ((cause as Error)?.name === "AbortError") throw cause;
    throw new BuilderStopped({
      status: 0,
      code: "offline",
      message: "Can't reach this app's server. Check your connection.",
    });
  }

  if (res.status === 401) notifyUnauthorized();
  const streaming = (res.headers.get("content-type") ?? "").includes("text/event-stream");
  if (!res.ok || !streaming || !res.body) {
    let body: unknown = null;
    try {
      body = await res.json();
    } catch {
      /* readRefusal copes: it produces a message from the status rather than inventing one */
    }
    throw new BuilderStopped(readRefusal(res.status, body));
  }

  const reader = res.body.pipeThrough(new TextDecoderStream()).getReader();
  const stream = new BuilderStream();
  let ended = false;
  try {
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      for (const frame of stream.feed(value)) {
        if (frame.kind !== "delta") ended = true;
        yield frame;
      }
    }
  } finally {
    await reader.cancel().catch(() => {});
  }

  // A stream that stops without saying why is not a finished answer. Saying so beats leaving a
  // half-written proposal on screen looking complete.
  if (!ended) {
    yield {
      kind: "error",
      code: "stream_broken",
      message: "The builder's answer stopped partway. Nothing was created; try again.",
    };
  }
}

// --- the builder's own credential ------------------------------------------

export interface BuilderCredentialStatus {
  configured: boolean;
  kind: "api_key" | "oauth_token" | null;
}

async function credentialRequest<T>(
  projectId: string,
  init: RequestInit & { expectVoid?: boolean },
): Promise<T> {
  const path = projectPath(projectId, "engine", "v1", "builder", "credential");
  if (path === null) throw new ApiError(404, "not_found", "That's gone, or was never yours.");
  const res = await fetch(`${PROXY}${path}`, {
    ...init,
    credentials: "same-origin",
    headers: { ...(init.headers ?? {}), "x-project-id": projectId },
  });
  if (res.status === 401) notifyUnauthorized();
  if (!res.ok) {
    let message = "The builder's credential could not be saved.";
    let code = `http_${res.status}`;
    try {
      const body = (await res.json()) as { error?: { code?: string; message?: string } };
      if (body?.error?.message) message = body.error.message;
      if (body?.error?.code) code = body.error.code;
    } catch {
      /* keep the default */
    }
    throw new ApiError(res.status, code, message);
  }
  if (init.expectVoid || res.status === 204) return undefined as T;
  return (await res.json()) as T;
}

export const builderCredential = {
  get: (projectId: string) =>
    credentialRequest<BuilderCredentialStatus>(projectId, { method: "GET" }),
  /** One of `api_key` or `setup_token`; the engine refuses a value that is not what it claims. */
  put: (projectId: string, body: { api_key?: string; setup_token?: string }) =>
    credentialRequest<BuilderCredentialStatus>(projectId, {
      method: "PUT",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(body),
    }),
  remove: (projectId: string) =>
    credentialRequest<void>(projectId, { method: "DELETE", expectVoid: true }),
};
