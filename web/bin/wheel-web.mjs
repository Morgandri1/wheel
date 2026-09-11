#!/usr/bin/env node

// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

/**
 * `npx wheel-web` — run the prebuilt board against a Wheel API.
 *
 * There is no build step here: the package ships Next's standalone output, and this only points
 * it at an API and starts it. The browser never talks to that API — this server proxies every
 * call (src/lib/api-proxy.ts) — so the API can listen on loopback only, and the URL is read by
 * the server when it starts rather than baked into a bundle.
 */
import { spawn } from "node:child_process";
import { existsSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const server = join(here, "..", "server.js");

const args = process.argv.slice(2);
if (args.includes("--help") || args.includes("-h")) {
  console.log(`
  wheel-web — the Wheel board, served locally.

  Usage
    npx wheel-web [--port <n>] [--api <url>] [--public-origin <url>]

  Options
    --port <n>             Port to listen on.                   (default 3000, or PORT)
    --api <url>            The Wheel API this server talks to.  (default http://127.0.0.1:8080, or WHEEL_API_URL)
    --public-origin <url>  The address browsers use, if not localhost (or WHEEL_PUBLIC_ORIGIN).
                           Without it the server answers on localhost only.

  The browser only ever talks to this server; the API can stay on loopback or a private network.
  Sign-in is the API's own email/password (WHEEL_AUTH_MODE=local) unless WHEEL_AUTH_MODE says otherwise.
`);
  process.exit(0);
}

/** A flag beats the environment, because it is the more specific thing the user just typed. */
function flag(name) {
  const i = args.indexOf(name);
  return i !== -1 && args[i + 1] ? args[i + 1] : undefined;
}

const port = flag("--port") ?? process.env.PORT ?? "3000";
// 127.0.0.1, not localhost: Node may resolve localhost to ::1 while the API listens on IPv4 only.
const apiUrl = flag("--api") ?? process.env.WHEEL_API_URL ?? "http://127.0.0.1:8080";
const authMode = process.env.WHEEL_AUTH_MODE || "local";
const publicOrigin = flag("--public-origin") ?? process.env.WHEEL_PUBLIC_ORIGIN;

try {
  // Fail on a malformed URL now, with a sentence, rather than on every request later.
  new URL(apiUrl);
} catch {
  console.error(`wheel-web: --api must be a URL, got ${JSON.stringify(apiUrl)}`);
  process.exit(1);
}

if (!existsSync(server)) {
  console.error("wheel-web: this package is missing its prebuilt server (server.js).");
  console.error("That means it was published wrong; please report it rather than working around it.");
  process.exit(1);
}

console.log(`wheel-web on http://localhost:${port}  →  API ${apiUrl} (reached from this server, never the browser)`);

const child = spawn(process.execPath, [server], {
  stdio: "inherit",
  env: {
    ...process.env,
    PORT: String(port),
    HOSTNAME: process.env.HOSTNAME ?? "0.0.0.0",
    WHEEL_API_URL: apiUrl,
    WHEEL_AUTH_MODE: authMode,
    ...(publicOrigin ? { WHEEL_PUBLIC_ORIGIN: publicOrigin } : {}),
  },
});

child.on("exit", (code, signal) => process.exit(signal ? 1 : (code ?? 0)));
for (const sig of ["SIGINT", "SIGTERM"]) process.on(sig, () => child.kill(sig));
