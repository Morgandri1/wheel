#!/usr/bin/env node

// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

/**
 * Fails when the web server's auth mode and the API's disagree.
 *
 * This is the one deployment error neither lane can see alone: both halves are individually
 * correct and the pair is broken, and the symptom is a user who cannot log in. Run it with the
 * web server's own environment, against the API it talks to, after a deploy or in CI.
 *
 *   WHEEL_API_URL=https://api.example WHEEL_AUTH_MODE=local node scripts/check-auth-mode.mjs
 *
 * The legacy NEXT_PUBLIC_ names are read as fallbacks, as the server itself does.
 * Exit 0 agree · 1 disagree · 2 could not tell (which is NOT a pass).
 */
import { authModeMismatch, serverAuthMode } from "../src/lib/auth-mode-check.ts";

const api = process.env.WHEEL_API_URL || process.env.NEXT_PUBLIC_API_URL;
const client = process.env.WHEEL_AUTH_MODE || process.env.NEXT_PUBLIC_AUTH_MODE || "mock";

if (!api) {
  console.error("WHEEL_API_URL is not set, so there is nothing to compare against.");
  process.exit(2);
}

const url = `${api.replace(/\/$/, "")}/healthz`;
let health;
try {
  const res = await fetch(url, { headers: { accept: "application/json" } });
  if (!res.ok) {
    console.error(`${url} answered ${res.status}. Cannot compare auth modes.`);
    process.exit(2);
  }
  health = await res.json();
} catch (e) {
  console.error(`${url} could not be reached: ${e instanceof Error ? e.message : e}`);
  process.exit(2);
}

const server = serverAuthMode(health);
if (!server) {
  // An older API predates the auth_mode field. Say so rather than pass: "I could not check" and
  // "I checked and it was fine" are different results and must not share an exit code.
  console.error(`${url} did not report an auth_mode. An API older than 52577ad cannot be checked.`);
  process.exit(2);
}

const mismatch = authModeMismatch(client, server);
if (mismatch) {
  console.error(`auth mode mismatch\n  ${mismatch}`);
  process.exit(1);
}
console.log(`auth mode agrees: web ${client} ↔ api ${server}`);
