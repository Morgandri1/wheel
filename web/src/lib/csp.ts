/**
 * Content Security Policy (ADVERSARY R7, binding).
 *
 * The session token lives in localStorage, which any script on this origin can read. CSP is what
 * keeps that tradeoff bounded: no inline script, no eval, and a nonce that only our own server
 * can mint per request. A board XSS would otherwise be a week-long account takeover.
 *
 * Two deliberate looseness decisions, both narrower than they look:
 *
 * `style-src 'unsafe-inline'` — server-rendered `style` attributes are subject to style-src, and
 * this app sets colours from CSS custom properties inline. The exposure is CSS injection, not
 * script execution, and CSP3's nonces do not apply to style attributes at all; `'unsafe-hashes'`
 * would need a hash per attribute and is not supported everywhere. Named here so it is a
 * decision rather than an oversight.
 *
 * `'unsafe-eval'` in development only — the dev server's React refresh runtime needs it. It is
 * absent from every production build, which is the one that ships.
 */
export function buildCsp({
  nonce,
  apiUrl,
  authMode,
  dev,
}: {
  nonce: string;
  apiUrl: string | undefined;
  authMode: string | undefined;
  dev: boolean;
}): string {
  const connect = new Set(["'self'"]);
  const origin = safeOrigin(apiUrl);
  if (origin) {
    connect.add(origin);
    // The events socket is the same origin over ws/wss; connect-src governs WebSocket too.
    connect.add(origin.replace(/^http/, "ws"));
  }
  if (dev) {
    // Dev server HMR, and whichever port the mock happens to be on.
    connect.add("ws://localhost:*");
    connect.add("http://localhost:*");
    connect.add("http://127.0.0.1:*");
  }

  const script = ["'self'", `'nonce-${nonce}'`, "'strict-dynamic'"];
  if (dev) script.push("'unsafe-eval'");

  const frame = ["'none'"];

  if (authMode === "clerk") {
    // Clerk loads its own script and talks to its own API; without these, clerk mode has no
    // sign-in at all. Listed only in the mode that uses them.
    script.push("https://*.clerk.accounts.dev", "https://*.clerk.com");
    connect.add("https://*.clerk.accounts.dev");
    connect.add("https://*.clerk.com");
    frame.length = 0;
    frame.push("https://*.clerk.accounts.dev", "https://*.clerk.com");
  }

  const directives = [
    "default-src 'self'",
    `script-src ${script.join(" ")}`,
    "style-src 'self' 'unsafe-inline'",
    "img-src 'self' data: blob:",
    "font-src 'self' data:",
    `connect-src ${[...connect].join(" ")}`,
    `frame-src ${frame.join(" ")}`,
    "worker-src 'self' blob:",
    "media-src 'none'",
    "manifest-src 'self'",
    "object-src 'none'",
    "base-uri 'none'",
    "form-action 'self'",
    "frame-ancestors 'none'",
  ];
  if (!dev) directives.push("upgrade-insecure-requests");

  return directives.join("; ");
}

/**
 * An origin, or nothing. A malformed NEXT_PUBLIC_API_URL must not be able to inject a directive:
 * the URL parser rejects anything with a space or a semicolon in it long before we join.
 */
function safeOrigin(url: string | undefined): string | null {
  if (!url) return null;
  try {
    const parsed = new URL(url);
    if (parsed.protocol !== "http:" && parsed.protocol !== "https:") return null;
    return parsed.origin;
  } catch {
    return null;
  }
}
