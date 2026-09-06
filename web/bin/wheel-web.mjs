#!/usr/bin/env node
/**
 * `npx wheel-web` — run the prebuilt board against a Wheel API.
 *
 * There is no build step here: the package ships Next's standalone output, and this only points
 * it at an API and starts it. The API URL is read at RUN time (see src/lib/runtime-config.ts);
 * baking it in would make this flag decorative, since NEXT_PUBLIC_* values are frozen when the
 * bundle is compiled.
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
    npx wheel-web [--port <n>] [--api <url>]

  Options
    --port <n>   Port to listen on.              (default 3000, or PORT)
    --api <url>  The Wheel API to talk to.       (default http://localhost:8080, or WHEEL_API_URL)

  The API URL is read when the server starts, so one prebuilt package works against any API.
  Sign-in is the API's own email/password: this build has no third-party identity provider in it.
`);
  process.exit(0);
}

/** A flag beats the environment, because it is the more specific thing the user just typed. */
function flag(name) {
  const i = args.indexOf(name);
  return i !== -1 && args[i + 1] ? args[i + 1] : undefined;
}

const port = flag("--port") ?? process.env.PORT ?? "3000";
const apiUrl = flag("--api") ?? process.env.WHEEL_API_URL ?? "http://localhost:8080";

try {
  // Fail on a malformed URL now, with a sentence, rather than letting every request in the
  // browser fail later with a CSP violation that names nothing.
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

console.log(`wheel-web on http://localhost:${port}  →  API ${apiUrl}`);

const child = spawn(process.execPath, [server], {
  stdio: "inherit",
  env: {
    ...process.env,
    PORT: String(port),
    HOSTNAME: process.env.HOSTNAME ?? "0.0.0.0",
    WHEEL_API_URL: apiUrl,
    NEXT_PUBLIC_AUTH_MODE: "local",
  },
});

child.on("exit", (code, signal) => process.exit(signal ? 1 : (code ?? 0)));
for (const sig of ["SIGINT", "SIGTERM"]) process.on(sig, () => child.kill(sig));
