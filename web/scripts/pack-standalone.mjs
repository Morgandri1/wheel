#!/usr/bin/env node
/**
 * Assemble the publishable `wheel-web` package from a standalone build.
 *
 * Next's standalone output is a server plus only the modules it traced, but it deliberately does
 * NOT copy `.next/static` or `public` — those are normally served by a CDN. For a package someone
 * runs locally there is no CDN, so they have to travel with it or every script and stylesheet
 * 404s and the board renders as unstyled HTML.
 */
import { cpSync, existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const web = join(dirname(fileURLToPath(import.meta.url)), "..");
const dist = join(web, "dist-pkg");
const built = join(web, process.env.NEXT_DIST_DIR ?? ".next-pkg");
const standalone = join(built, "standalone");

if (!existsSync(standalone)) {
  console.error(`pack: no standalone build at ${standalone}`);
  console.error("Run: WHEEL_STANDALONE=1 NEXT_PUBLIC_AUTH_MODE=local NEXT_DIST_DIR=.next-pkg next build");
  process.exit(1);
}

rmSync(dist, { recursive: true, force: true });
mkdirSync(dist, { recursive: true });

cpSync(standalone, dist, { recursive: true });

/**
 * Static assets go under the SAME dist-dir name the build used, not a hardcoded ".next".
 *
 * The server resolves /_next/static/* against its configured distDir, so building with
 * NEXT_DIST_DIR=.next-pkg and copying into .next/static produces a package that renders perfectly
 * and hydrates never: the HTML is server-rendered, every chunk 404s, and React never takes over.
 * Nothing about that looks broken until you click something.
 */
const distDirName = process.env.NEXT_DIST_DIR ?? ".next";
cpSync(join(built, "static"), join(dist, distDirName, "static"), { recursive: true });
if (existsSync(join(web, "public"))) cpSync(join(web, "public"), join(dist, "public"), { recursive: true });
mkdirSync(join(dist, "bin"), { recursive: true });
cpSync(join(web, "bin", "wheel-web.mjs"), join(dist, "bin", "wheel-web.mjs"));

const source = JSON.parse(readFileSync(join(web, "package.json"), "utf8"));
// The published manifest is written, not copied: the app's devDependencies and scripts have no
// business in a package whose only job is to run an already-built server.
writeFileSync(
  join(dist, "package.json"),
  JSON.stringify(
    {
      name: "wheel-web",
      version: process.env.WHEEL_WEB_VERSION ?? source.version,
      description: "The Wheel board, prebuilt. Point it at a Wheel API and open a browser.",
      bin: { "wheel-web": "./bin/wheel-web.mjs" },
      // NOT "type": "module". Next's standalone server.js is CommonJS and calls require(); with
      // the package marked ESM, Node refuses it on the first line. The bin is .mjs, which is ESM
      // by extension and needs no help from here. Found by running the packed CLI, not by
      // reading it — the manifest looked perfectly reasonable.
      engines: { node: ">=20" },
      license: source.license ?? "UNLICENSED",
    },
    null,
    2,
  ) + "\n",
);

/**
 * Assert the package is actually runnable before anyone publishes it.
 *
 * Every failure this script can produce is silent at pack time and obvious only to a user, so the
 * cheap checks belong here: a missing chunk directory is a dead page, and a `type: module` in the
 * manifest stops Next's CommonJS server on its first line.
 */
const chunks = join(dist, distDirName, "static", "chunks");
if (!existsSync(chunks)) {
  console.error(`pack: no client chunks at ${chunks} — the package would render but never hydrate.`);
  process.exit(1);
}
const manifest = JSON.parse(readFileSync(join(dist, "package.json"), "utf8"));
if (manifest.type === "module") {
  console.error("pack: manifest says type=module, which stops Next's CommonJS server.js.");
  process.exit(1);
}
// The repo's own package.json is private and has no `bin` — correct, since it is never published
// and its bin would point at a server.js that does not exist in the source tree. The PUBLISHED
// manifest is the one that needs it, and that distinction is easy to miss by reading either file
// alone, so it is asserted rather than explained.
if (!manifest.bin?.["wheel-web"] || !existsSync(join(dist, manifest.bin["wheel-web"]))) {
  console.error("pack: the published manifest needs a `bin` pointing at a file that exists;");
  console.error(`      got ${JSON.stringify(manifest.bin)} — npx would resolve to nothing.`);
  process.exit(1);
}

console.log(`packed → ${dist}`);
console.log(`  client chunks at ${distDirName}/static/chunks`);
