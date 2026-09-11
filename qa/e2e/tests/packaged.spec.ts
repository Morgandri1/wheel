// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import { test, expect } from "@playwright/test";
import { T } from "../testids";
import { expectHydrated } from "../hydration";

/**
 * E2E-pkg-* — the artifact users actually install (`npx wheel-web`), not the one we develop
 * against.
 *
 * Everything else in this suite runs `next dev`. Users run a prebuilt standalone bundle
 * started by bin/wheel-web.mjs, and the two differ in exactly the ways that do not show up
 * until someone installs it: assets missing from the package, a build-time constant frozen
 * where a runtime value was intended, a CSP computed for a different origin.
 *
 * Web named this gap and asked for the coverage. The load-bearing claim is `--api <url>`:
 * the natural implementation of a flag like that is one that does nothing whatsoever, and it
 * would look completely fine in review. The package here is started against a mock on 8789
 * while its own default is 127.0.0.1:8080, where nothing listens — so if the flag were
 * decorative, sign-up fails with "can't reach the API" rather than passing quietly.
 *
 * Since web/server-side-api the browser never calls the API at all; the package's server
 * does. So "which API" is proven by an outcome, and "not the browser" by the request log.
 *
 * KNOWN LIMITATION, recorded rather than glossed. I checked this suite CAN fail, by starting
 * the package against the wrong api. It does fail — but by TIMING OUT rather than by hitting
 * the assertion, so the report says "test timeout" instead of "these calls went to :8787".
 * The negative case is slower and far less legible than it should be, and whoever debugs a
 * real regression will get a worse message than they deserve. Worth fixing; not fixed.
 *
 * Two things that first attempt taught me, both fixed:
 *   - `reuseExistingServer` made the check VACUOUS. Playwright found the server left over
 *     from the previous run — started with the RIGHT flag — and used that, so the sabotage
 *     never reached the code and both tests passed. This suite tests HOW THE SERVER WAS
 *     LAUNCHED, so it may never reuse one.
 *   - Even with reuse off, a stale process holding :3300 makes the new server fail to bind
 *     while the URL check passes against the old one — the same vacuum by another route.
 *     `make test-pkg` frees the port first.
 */

const PKG_API = process.env.WHEEL_PKG_API_URL ?? "http://localhost:8789";

test("E2E-pkg-runtime-api: --api is honoured at run time by the server, and the browser never calls it", async ({ page }) => {
  const calls: string[] = [];
  page.on("request", (r) => calls.push(r.url()));

  // `domcontentloaded`, not the default `load`: Next aborts its own RSC prefetches on this
  // page, and a page with an aborted request never fires `load`, so the default wait hangs
  // until the test times out and reports nothing useful about the package.
  await page.goto("/sign-up", { waitUntil: "domcontentloaded" });

  // The package's server is the only thing that talks to the API now, so WHICH api is proven
  // by an outcome only the right one can produce: signing up succeeds only if the server
  // reached the mock on :8789. With a decorative --api it would try its default, 127.0.0.1:8080,
  // where nothing listens, and the form would say "can't reach the API" instead.
  const email = `packaged-${Date.now()}@example.test`;
  await page.getByTestId(T.emailInput).fill(email);
  await page.getByTestId(T.passwordInput).fill("correct-horse-battery");
  // noWaitAfter: a failing server answers the form instead of navigating, and awaiting a
  // navigation that never comes would HANG until the test's own timeout. A gate should fail
  // fast and say why; a hang says nothing and costs a minute to say it.
  await page.getByTestId(T.authSubmit).click({ noWaitAfter: true });
  await expect(page.getByTestId(T.sessionBadge)).toContainText(email, { timeout: 20_000 });

  // And the browser went nowhere but its own server: not to PKG_API, not to anything else.
  const origin = new URL(page.url()).origin;
  const strays = calls.filter((u) => new URL(u).origin !== origin);
  expect(strays, "the browser talked to something other than the package's own server").toEqual([]);
});

test("E2E-pkg-csp-agrees: the CSP lets the page reach its own server and names no API", async ({ page }) => {
  const res = await page.goto("/app", { waitUntil: "domcontentloaded" });
  const csp =
    res?.headers()["content-security-policy"] ?? res?.headers()["content-security-policy-report-only"];

  // The browser has no business with the API, so the policy says so: connect-src is the
  // page's own origin and nothing else, and the address the server was given never appears.
  expect(csp, "the packaged server sent no CSP at all").toBeTruthy();
  expect(/connect-src ([^;]*)/.exec(csp!)?.[1]?.trim()).toBe("'self'");
  expect(csp).not.toContain(PKG_API);
});

test("E2E-pkg-assets: the package ships the assets it references", async ({ page }) => {
  const missing: string[] = [];
  page.on("response", (r) => {
    if (r.status() === 404) missing.push(new URL(r.url()).pathname);
  });
  const errors: string[] = [];
  page.on("console", (m) => {
    if (m.type() === "error") errors.push(m.text());
  });

  await page.goto("/app", { waitUntil: "domcontentloaded" });
  await page.waitForTimeout(3000);

  // `next dev` serves from source and will happily find a file the packer never copied.
  // A 404 on /_next/static or /monaco here is a packaging bug that no other test can see,
  // and it degrades quietly: the board renders, one panel is just dead.
  expect(missing, "the packaged server 404'd on its own assets").toEqual([]);
  expect(errors).toEqual([]);
});

test("E2E-pkg-hydrates: the packaged board is interactive, not merely rendered", async ({ page }) => {
  await page.goto("/app", { waitUntil: "domcontentloaded" });
  // "Renders" is not "works" — a bundle that ships but never hydrates serves perfect HTML
  // and responds to nothing. Proving hydration needs a control whose state only exists
  // once React is live.
  const signIn = page
    .getByTestId(T.authForm)
    .or(page.getByTestId(T.projectNew))
    .or(page.getByTestId(T.projectNewEmpty));
  // Was toBeVisible + toBeEnabled. Both pass on server-rendered HTML with the bundle
  // missing — I wrote the comment about "renders is not works" and then asserted exactly
  // that. A packaged build is the likeliest place for a bundle to be absent, so this is
  // the spec that could least afford it.
  await expectHydrated(signIn.first(), "the packaged board's entry control");
});
