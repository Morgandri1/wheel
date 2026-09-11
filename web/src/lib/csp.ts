// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

/**
 * Content Security Policy (ADVERSARY R7, binding).
 *
 * The session is an httpOnly cookie, so script cannot read it — but script running on this origin
 * can still USE it, by calling this app's routes while the page is open. CSP is what keeps that
 * bounded: no inline script, no eval, and a nonce that only our own server can mint per request.
 *
 * `connect-src` is `'self'` and nothing else. The browser talks only to this app's server, which
 * reaches the API itself; naming the API here would publish an address the browser never needs.
 * Clerk mode adds Clerk's own hosts, because Clerk's script talks to Clerk.
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
  authMode,
  dev,
}: {
  nonce: string;
  authMode: string | undefined;
  dev: boolean;
}): string {
  const connect = ["'self'"];
  // The dev server's hot-reload socket.
  if (dev) connect.push("ws://localhost:*", "ws://127.0.0.1:*");

  // 'strict-dynamic' turns off host allowlisting, so 'self' stops meaning anything and every
  // script must be nonced or loaded by a nonced one. That is what we want in production — and it
  // is wrong in development, where Next serves its error overlay from un-nonced fallback chunks.
  // With the policy on, a missing module rendered as a BLANK PAGE instead of Next's error, which
  // cost real debugging time. Dev keeps the nonce and 'self'; production keeps 'strict-dynamic'.
  const script = ["'self'", `'nonce-${nonce}'`];
  if (dev) script.push("'unsafe-eval'");
  else script.push("'strict-dynamic'");

  const frame = ["'none'"];

  if (authMode === "clerk") {
    // Clerk loads its own script and talks to its own API; without these, clerk mode has no
    // sign-in at all. Listed only in the mode that uses them.
    script.push("https://*.clerk.accounts.dev", "https://*.clerk.com");
    connect.push("https://*.clerk.accounts.dev", "https://*.clerk.com");
    frame.length = 0;
    frame.push("https://*.clerk.accounts.dev", "https://*.clerk.com");
  }

  const directives = [
    "default-src 'self'",
    `script-src ${script.join(" ")}`,
    "style-src 'self' 'unsafe-inline'",
    "img-src 'self' data: blob:",
    "font-src 'self' data:",
    `connect-src ${connect.join(" ")}`,
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
