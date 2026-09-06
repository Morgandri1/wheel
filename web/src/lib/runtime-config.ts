/**
 * Where the API lives, resolved at RUN time rather than baked at build time.
 *
 * `NEXT_PUBLIC_*` values are inlined into the bundle when it is compiled, which is fine for a
 * deployment we build ourselves and useless for a prebuilt package: `npx wheel-web` ships an
 * already-compiled bundle, so a user setting WHEEL_API_URL in their shell would be setting a
 * variable nothing reads. A package whose single option silently does nothing is worse than one
 * that has no option at all.
 *
 * So the server resolves the URL per request and hands it to the client, which records it here
 * before anything fetches. The build-time value remains the fallback, which is what keeps the
 * Vercel deployment working exactly as before.
 */

const BUILD_TIME_DEFAULT = (process.env.NEXT_PUBLIC_API_URL ?? "http://localhost:8787").replace(
  /\/$/,
  "",
);

let runtimeValue: string | null = null;

/** Called once by the client shim with whatever the server resolved for this request. */
export function setApiBaseUrl(url: string | null | undefined) {
  if (typeof url === "string" && url.trim()) runtimeValue = url.trim().replace(/\/$/, "");
}

export function apiBaseUrl(): string {
  return runtimeValue ?? BUILD_TIME_DEFAULT;
}

/**
 * Server-side resolution, shared by the root layout and the middleware so the page and the CSP
 * can never disagree about which API is in use — a mismatch there blocks every call in the
 * browser while looking like a network fault.
 *
 * WHEEL_API_URL wins because it is the runtime knob; NEXT_PUBLIC_API_URL is the build-time one a
 * self-built deployment already sets.
 */
export function serverApiBaseUrl(): string {
  // Truthiness, not `??`. `WHEEL_API_URL=` with nothing after it is ordinary in a shell script,
  // a Dockerfile or a CI matrix, and it is not nullish — so `??` would accept the empty string
  // and every request would go to a relative path against the app's own origin. Same trap that
  // rendered an endpoint's public URL as a bare "/hook".
  const value = firstNonEmpty(process.env.WHEEL_API_URL, process.env.NEXT_PUBLIC_API_URL);
  return (value ?? "http://localhost:8787").replace(/\/$/, "");
}

function firstNonEmpty(...values: (string | undefined)[]): string | undefined {
  for (const value of values) if (typeof value === "string" && value.trim()) return value.trim();
  return undefined;
}
