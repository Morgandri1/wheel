// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

/**
 * Hitting an endpoint's public URL from the browser, so "reachable" is a measurement.
 *
 * The panel used to assert that endpoints were reachable whenever the `http` capability was on.
 * That is a claim about configuration, not about the world: the engine may not serve ingress yet,
 * in which case the URL answers 404 and the panel was confidently wrong.
 */
export type Probe =
  | {
      kind: "answered";
      status: number;
      statusText: string;
      body: string;
      truncated: boolean;
      /** `error.code` from the API's envelope, when it sent one. A bare status carries no code. */
      code: string | null;
    }
  | { kind: "unreadable"; reason: string };

const BODY_LIMIT = 2000;

/**
 * A browser reports a CORS refusal and a dead host identically — a TypeError with no status, by
 * design, so a page cannot use fetch to probe what it is not allowed to see. Reporting either as
 * "unreachable" would invent a fact we do not have.
 */
export function unreadableReason(error: unknown): string {
  const detail = error instanceof Error && error.message ? ` (${error.message})` : "";
  return `No readable response${detail}. The request failed, or the API did not allow this page to read the reply — the browser does not say which, so this is not evidence that the endpoint is down.`;
}

/** The API's `error.code`, if the body is its envelope. Anything else is not an error envelope. */
/**
 * Two shapes reach this parser and both are real.
 *
 * The API wraps errors in `wheel_core::ErrorBody` — `{"error":{"code","message"}}`. The engine's
 * ingress does not: its local `err()` helper (wheel-engine/src/api/ingress.rs) emits a bare
 * `{"code":"no_such_endpoint"}`. Reading only the wrapped form made the "check your path" verdict
 * unreachable on a live board, which is the one state an operator most needs named.
 */
export function errorCode(body: string): string | null {
  try {
    const parsed = JSON.parse(body) as { error?: { code?: unknown }; code?: unknown };
    const code = parsed?.error?.code ?? parsed?.code;
    return typeof code === "string" && code ? code : null;
  } catch {
    return null;
  }
}

/**
 * How many rows ingress wrote for this hit.
 *
 * The engine now sends `queued`; engines before 168430f sent `delivered` for the same number.
 * Both count rows ENQUEUED — a stopped or parked agent still counts — so both read identically.
 * `delivered` is a defined state in the protocol (§3c #4: the bytes reached the child's stdin) and
 * the 202 is written before any child is touched, which is why the engine stopped claiming it.
 */
export function queuedCount(body: string): number | null {
  try {
    const parsed = JSON.parse(body) as { queued?: unknown; delivered?: unknown };
    const n = parsed?.queued ?? parsed?.delivered;
    return typeof n === "number" ? n : null;
  } catch {
    return null;
  }
}

/**
 * What a status actually tells you. Deliberately short of a verdict it cannot support.
 *
 * These are four DIFFERENT problems with four different fixes, and the operator hit two of them
 * in one afternoon while trying to work out whether their own URL was wrong:
 *   403 -> the project capability is off        (fix: the Enable button in this panel)
 *   501 -> this engine predates ingress          (fix: restart the project)
 *   404 -> a real answer about this path         (fix: the path)
 *   202 -> accepted and queued                   (nothing to fix here; the agent may still be parked)
 * A panel that renders them as one shade of failure is what sent them looking at the path.
 */
export function probeVerdict({
  status,
  code = null,
  body = "",
}: {
  status: number;
  code?: string | null;
  body?: string;
}): string {
  if (code === "ingress_unavailable") {
    return "This project's engine predates endpoint ingress — restart the project to pick it up.";
  }
  if (code === "no_such_endpoint") {
    return "The engine is serving ingress but has no endpoint at this path — check the path above.";
  }
  if (status === 403) return "Reached the API, which refused it — public HTTP is off for this project.";
  if (status === 202 || (status >= 200 && status < 300)) {
    const queued = queuedCount(body);
    if (queued === 0) {
      return "Ingress accepted it, but nothing is wired to this endpoint, so it was dropped.";
    }
    if (queued !== null) {
      const nodes = `${queued} wired node${queued === 1 ? "" : "s"}`;
      return `Ingress accepted it and queued it for ${nodes}. Queued is not read: a parked or stopped agent picks it up when it next runs.`;
    }
    return "The endpoint answered.";
  }
  if (code === "not_found") {
    // Verified against production: the API answers this when no project has that id, BEFORE it
    // looks at the path or the capability. Sending the operator to check their path here would be
    // the same wrong turn the 501 copy exists to prevent.
    return "The API has no project with this id — it may have been deleted. This is not about the path.";
  }
  if (status === 404) {
    // Retires itself: API turns a BODILESS 404 into 501, so once every engine is current this arm
    // is unreachable. A 404 the engine WROTE is a real answer about the path and must not be
    // excused as a missing feature.
    return body.trim()
      ? "Something answered 404, with a body. That is a real answer about this path, not a missing feature — read it below."
      : "Reached the API, which served nothing here and said nothing about why — most likely an engine that predates ingress. Restarting the project is the first thing to try.";
  }
  if (status === 405) return "Reached the ingress; this path does not accept that method.";
  if (status === 501) return "This project's engine predates endpoint ingress — restart the project to pick it up.";
  if (status === 429) return "Reached the API and was rate-limited.";
  if (status >= 500) return "Reached the API; it failed on the way to the endpoint.";
  return "The API answered.";
}

/**
 * Never sends credentials: this URL is public by definition and a session token has no business
 * on it. `no-store` so a cached 404 cannot be mistaken for a live measurement.
 */
export async function probeEndpoint(
  url: string,
  { method = "GET", fetchImpl = fetch }: { method?: string; fetchImpl?: typeof fetch } = {},
): Promise<Probe> {
  // The endpoint's OWN method, not always GET: ingress routes on the method, so a GET against a
  // POST endpoint can only ever produce 404/405 and the delivered-202 state would be unreachable
  // from the one button that exists to show it.
  const sendsBody = method !== "GET" && method !== "HEAD";
  try {
    const res = await fetchImpl(url, {
      method,
      credentials: "omit",
      cache: "no-store",
      ...(sendsBody
        ? { headers: { "content-type": "application/json" }, body: JSON.stringify({ source: "wheel-endpoint-test" }) }
        : {}),
    });
    const text = await res.text().catch(() => "");
    return {
      kind: "answered",
      status: res.status,
      statusText: res.statusText,
      body: text.slice(0, BODY_LIMIT),
      truncated: text.length > BODY_LIMIT,
      code: errorCode(text),
    };
  } catch (error) {
    return { kind: "unreadable", reason: unreadableReason(error) };
  }
}
