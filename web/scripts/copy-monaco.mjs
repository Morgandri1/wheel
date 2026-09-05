import { cp, mkdir, rm, stat } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

/**
 * Serve Monaco from our own origin.
 *
 * `@monaco-editor/react` fetches the editor from jsDelivr by default. That means a third party
 * can serve executable code into our origin, where it can read the session token out of
 * localStorage — a supply-chain hole a strict CSP is supposed to close, and one we only noticed
 * because the policy blocked Monaco's stylesheet while 'strict-dynamic' let its script through.
 * Copying the editor into public/ makes 'self' the whole story.
 *
 * Plain .mjs on purpose: this runs in `prebuild`, so anything it imports becomes a BUILD
 * dependency. As TypeScript it needed `tsx`, which is a devDependency — and a deploy that
 * installs production dependencies only would then fail at build time, for a script whose whole
 * job is copying files. Node runs this as-is, with nothing installed.
 *
 * public/monaco is generated, not committed.
 */
const here = dirname(fileURLToPath(import.meta.url));
const from = resolve(here, "../node_modules/monaco-editor/min/vs");
const to = resolve(here, "../public/monaco/vs");

try {
  await stat(from);
} catch {
  console.error(`monaco-editor is not installed at ${from} — run pnpm install`);
  process.exit(1);
}

await rm(to, { recursive: true, force: true });
await mkdir(dirname(to), { recursive: true });
// Translations are 2 MB of files the editor never requests — it runs in English and only fetches
// a locale bundle if one is configured, which we do not do.
await cp(from, to, { recursive: true, filter: (src) => !/nls\.messages\.[\w-]+\.js$/.test(src) });
console.log("monaco → public/monaco/vs");
