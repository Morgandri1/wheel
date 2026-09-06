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
export function errorCode(body: string): string | null {
  try {
    const parsed: unknown = JSON.parse(body);
    const code = (parsed as { error?: { code?: unknown } })?.error?.code;
    return typeof code === "string" && code ? code : null;
  } catch {
    return null;
  }
}

/**
 * What a status actually tells you. Deliberately short of a verdict it cannot support.
 *
 * The case that matters is a bare 404. Endpoint ingress does not exist engine-side yet, so today
 * EVERY board 404s — and the operator who hit that on `/tg` could not tell "not built" from
 * "I typed the path wrong". A 404 with no code is evidence about the engine, not about the path,
 * and saying so is the whole point of this button.
 */
export function probeVerdict(status: number, code: string | null = null): string {
  if (code === "ingress_unavailable") return "This project's engine does not serve endpoints yet.";
  if (status === 403) return "Reached the API, which refused it — public HTTP is off for this project.";
  if (status === 404) {
    return "Reached the API, which served nothing here. Endpoint ingress is not built yet, so a bare 404 is expected on every board today — it does not mean your path is wrong.";
  }
  if (status === 405) return "Reached the ingress; this path does not accept GET.";
  if (status === 501) return "Reached the API; ingress is not implemented yet.";
  if (status === 429) return "Reached the API and was rate-limited.";
  if (status >= 500) return "Reached the API; it failed on the way to the endpoint.";
  if (status >= 200 && status < 300) return "The endpoint answered.";
  return "The API answered.";
}

/**
 * Never sends credentials: this URL is public by definition and a session token has no business
 * on it. `no-store` so a cached 404 cannot be mistaken for a live measurement.
 */
export async function probeEndpoint(url: string, fetchImpl: typeof fetch = fetch): Promise<Probe> {
  try {
    const res = await fetchImpl(url, { method: "GET", credentials: "omit", cache: "no-store" });
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
