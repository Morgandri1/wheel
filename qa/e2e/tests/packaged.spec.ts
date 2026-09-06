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
 * NEXT_PUBLIC_* values are inlined when the bundle compiles, so the natural implementation
 * of that flag is one that does nothing whatsoever, and it would look completely fine in
 * review. The package here is started against a mock on 8789 while the build-time default
 * is 8787 — so if the flag were decorative, every assertion below fails on an empty board
 * rather than passing quietly.
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

test("E2E-pkg-runtime-api: --api is honoured at run time, not frozen at build time", async ({ page }) => {
  const calls: string[] = [];
  page.on("request", (r) => {
    if (r.url().includes("/v1/")) calls.push(r.url());
  });

  // `domcontentloaded`, not the default `load`: Next aborts its own RSC prefetches on this
  // page, and a page with an aborted request never fires `load`, so the default wait hangs
  // until the test times out and reports nothing useful about the package.
  await page.goto("/app", { waitUntil: "domcontentloaded" });

  // The packaged build is AUTH_MODE=local, so /app redirects to /sign-in and an
  // unauthenticated board calls no API at all. Asserting on "some /v1/ request happened"
  // was unsatisfiable by construction — it would have hung here forever waiting for a call
  // this page never makes. Signing in is what actually produces one.
  await expect(page).toHaveURL(/\/sign-in/);
  await page.getByTestId(T.emailInput).fill("packaged@example.test");
  await page.getByTestId(T.passwordInput).fill("correct-horse-battery");
  // noWaitAfter: with a wrong --api the submit targets an origin nothing is listening on,
  // and awaiting the navigation makes the test HANG until its own timeout instead of
  // failing on the assertion. A gate should fail fast and say why; a hang says nothing and
  // costs a minute to say it.
  await page.getByTestId(T.authSubmit).click({ noWaitAfter: true });

  await expect.poll(() => calls.length, { timeout: 20_000 }).toBeGreaterThan(0);

  // WHICH api, not THAT an api. The build-time default is :8787; this package was started
  // with --api :8789. A single call to 8787 means the flag is decorative. The outcome of
  // the sign-in is irrelevant — a 401 proves the routing just as well as a 200.
  const strays = calls.filter((u) => !u.startsWith(PKG_API));
  expect(strays, `these went somewhere other than ${PKG_API}`).toEqual([]);
});

test("E2E-pkg-csp-agrees: the CSP allows the API the server was pointed at", async ({ page }) => {
  const res = await page.goto("/app", { waitUntil: "domcontentloaded" });
  const csp =
    res?.headers()["content-security-policy"] ?? res?.headers()["content-security-policy-report-only"];

  // A CSP computed for a different origin than the one the app calls blocks every request
  // in the browser and reports it as a violation, not a failed fetch — so it reads as a
  // network fault and gets debugged in the wrong place entirely. The page and the policy
  // have to be derived from the same resolved value, and this is what proves they are.
  expect(csp, "the packaged server sent no CSP at all").toBeTruthy();
  expect(csp).toContain(PKG_API);
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
