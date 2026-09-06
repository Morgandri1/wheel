/**
 * E2E — local email/password auth. TESTPLAN E2E-local-*, and the browser half of the
 * AUTH-local-* criteria in §7a.
 *
 * ADOPTED FROM WEB (their 10-test spec, handed over rather than left in /tmp). Kept
 * substantially as they wrote it — they know their own UI — with four changes:
 *
 *  1. Selectors go through `T`, so `qa/contract/testid_parity.py` fails in under a second
 *     when a name drifts, instead of a browser launch failing 30s in.
 *  2. Added the assertion `E2E-local-bad-password` is actually about: the wrong-password
 *     message must be byte-identical to the unknown-account message. Web's version asserted
 *     that an error is SHOWN; the S1 criterion is that it does not distinguish the two.
 *     A helpful "no account with that email" is the enumeration oracle we are most likely
 *     to reintroduce, and it reappears in the client long after the API stops leaking it.
 *  3. Added the three-state gate as a test, because Web flagged it as the thing worth
 *     keeping and an invariant nobody asserts is an invariant that regresses. Collapsing
 *     `loading` into `anon` bounces every returning user to /sign-in for one frame — which
 *     arrives as an intermittent bug report, the worst kind to receive.
 *  4. URLs come from the config's baseURL rather than a literal, so WHEEL_WEB_URL still
 *     works and the local-mode server's port lives in one place.
 *
 * This runs as its own Playwright project (`local-auth`) against its own dev server on
 * 3200: NEXT_PUBLIC_AUTH_MODE is inlined at build time, so local mode cannot share the
 * default suite's mock-mode server on 3000.
 */
import { expect, test, type Page } from "@playwright/test";
import { T } from "../testids";
import { SEEDED_SHAPE, SESSION_KEY } from "../session";

const API = process.env.WHEEL_LOCAL_API_URL ?? "http://localhost:8788";
const SEEDED = { email: "dev@wheel.dev", password: "wheel-dev-password" };

test.describe.configure({ mode: "serial" });

async function submit(page: Page, email: string, password: string) {
  await page.getByTestId(T.emailInput).fill(email);
  await page.getByTestId(T.passwordInput).fill(password);
  await page.getByTestId(T.authSubmit).click();
}

test("E2E-local-signin-redirect: a signed-out visitor to /app is taken to sign in", async ({ page }) => {
  await page.goto("/app");
  await page.waitForURL(/\/sign-in/);
  await expect(page.getByTestId(T.authScreen)).toBeVisible();
});

test("E2E-local-deep-link: a deep link keeps its destination through the round trip", async ({ page }) => {
  await page.goto("/app/some-project-id");
  await page.waitForURL(/\/sign-in\?next=/);
  expect(new URL(page.url()).searchParams.get("next")).toBe("/app/some-project-id");
});

test("E2E-local-bad-password: wrong credentials say so, and do not sign anyone in", async ({ page }) => {
  await page.goto("/sign-in");
  await submit(page, SEEDED.email, "definitely-wrong");
  await expect(page.getByTestId(T.authError)).toBeVisible();
  expect(page.url()).toContain("/sign-in");
  expect(await page.evaluate(() => window.localStorage.getItem("wheel.session"))).toBeNull();
  // Emptied on failure so a retry is not a half-edit of a wrong value.
  await expect(page.getByTestId(T.passwordInput)).toHaveValue("");
});

test("E2E-local-no-enumeration: the browser cannot tell a wrong password from no account", async ({ page }) => {
  await page.goto("/sign-in");
  await submit(page, SEEDED.email, "definitely-wrong");
  await expect(page.getByTestId(T.authError)).toBeVisible();
  const wrongPassword = (await page.getByTestId(T.authError).textContent())?.trim();

  await page.goto("/sign-in");
  await submit(page, `no-such-account-${Date.now()}@wheel.dev`, "definitely-wrong");
  await expect(page.getByTestId(T.authError)).toBeVisible();
  const noSuchUser = (await page.getByTestId(T.authError).textContent())?.trim();

  // AUTH-local-wrong-password / AUTH-local-no-such-user, seen from the browser. The
  // string itself does not matter; that the two are the same string does.
  expect(noSuchUser).toBe(wrongPassword);
  expect(noSuchUser).not.toMatch(/no account|not found|unknown|does not exist|unregistered/i);
  // Equality is only evidence while E2E-local-error-plumbing is green: see the note there.
  expect(noSuchUser).toBeTruthy();
});

test("E2E-local-error-plumbing: the API's own message reaches the UI, not a fallback", async ({ page }) => {
  // This exists because E2E-local-no-enumeration cannot stand on its own. It asserts that
  // the wrong-password and unknown-account strings are EQUAL — and two copies of the client's
  // generic fallback are equal. Web's mock once answered {error:"<string>"} where API.md says
  // {error:{code,message}}, so every server message was discarded and the UI fell back; the
  // enumeration test was green throughout, asserting nothing, and would have gone RED the day
  // someone fixed the plumbing. That is the shape of test people learn to delete.
  //
  // It cannot be rescued by matching copy: local-auth.ts's 401 fallback is byte-identical to
  // the server's 401 message, deliberately. So the only way to prove the pathway is live is to
  // put a value on the wire that the fallback could not invent.
  const sentinel = `server-said-${Date.now()}`;
  await page.route(`${API}/v1/auth/login`, (route) =>
    route.fulfill({
      status: 401,
      contentType: "application/json",
      body: JSON.stringify({ error: { code: "invalid_credentials", message: sentinel } }),
    }),
  );
  await page.goto("/sign-in");
  await submit(page, SEEDED.email, "definitely-wrong");
  await expect(page.getByTestId(T.authError)).toHaveText(sentinel);
});

test("E2E-local-pw-policy: the password rule is taught before a round trip", async ({ page }) => {
  let posted = 0;
  await page.route(`${API}/v1/auth/signup`, (route) => {
    posted += 1;
    return route.continue();
  });
  await page.goto("/sign-up");
  await submit(page, "brand-new@wheel.dev", "short");
  await expect(page.getByText(/at least 10 characters/i).first()).toBeVisible();
  // Counted requests, not DOM: a message that appears while the request goes out anyway
  // is not client-side validation, and only the request count can tell the difference.
  expect(posted).toBe(0);
});

test("E2E-local-signup: signing up lands on the board and names you in the header", async ({ page }) => {
  const email = `e2e-${Date.now()}@wheel.dev`;
  await page.goto("/sign-up");
  await submit(page, email, "a-long-enough-password");
  await page.waitForURL(/\/app$/);
  await expect(page.getByTestId(T.sessionBadge)).toContainText(email);
});

test("E2E-local-login: signing in returns you to the board, and survives a reload", async ({ page }) => {
  await page.goto("/app");
  await page.waitForURL(/\/sign-in/);
  await submit(page, SEEDED.email, SEEDED.password);
  await page.waitForURL(/\/app$/);
  await expect(page.getByTestId(T.sessionBadge)).toContainText(SEEDED.email);

  await page.reload();
  await expect(page.getByTestId(T.sessionBadge)).toContainText(SEEDED.email);
  expect(page.url()).toContain("/app");
});

test("E2E-local-session-gate: a returning user is never shown as signed out first", async ({ page }) => {
  await page.goto("/sign-in");
  await submit(page, SEEDED.email, SEEDED.password);
  await page.waitForURL(/\/app$/);

  // Three states, not two: `loading` (storage unread) is distinct from `anon`. If they are
  // ever collapsed, a returning user is bounced to /sign-in for a frame — an intermittent
  // report nobody can reproduce. Assert we never reach the signed-out state on the way back.
  const redirected: string[] = [];
  page.on("framenavigated", (f) => {
    if (f === page.mainFrame()) redirected.push(f.url());
  });
  await page.reload();
  await expect(page.getByTestId(T.sessionBadge)).toBeVisible();
  expect(redirected.filter((u) => u.includes("/sign-in"))).toEqual([]);
  await expect(page.getByTestId(T.sessionRedirecting)).toHaveCount(0);
});

test("E2E-local-session-shape: what a real sign-in stores is what seedSession fakes", async ({ page }) => {
  // The guard for qa/e2e/session.ts. Other specs seed a session instead of driving this
  // form, which is steadier and keeps sign-in failures in one place — but a seeded session
  // is a fake of the app's own state, and a fake nobody compares to the real thing drifts.
  // If the stored shape ever gains a field, or nests the user, or renames the token, every
  // seeded spec keeps passing against a shape the app stopped producing. This is the test
  // that makes the seed safe to rely on, so it must sign in FOR REAL.
  await page.goto("/sign-in");
  await submit(page, SEEDED.email, SEEDED.password);
  await page.waitForURL(/\/app$/);

  const stored = await page.evaluate((k) => {
    const raw = window.localStorage.getItem(k);
    return raw ? (JSON.parse(raw) as Record<string, unknown>) : null;
  }, SESSION_KEY);

  expect(stored, "a real sign-in stored no session at all").toBeTruthy();

  // Compared as SHAPE, not value: keys and types, since the token and id are per-session.
  const shapeOf = (o: unknown): unknown =>
    o && typeof o === "object" && !Array.isArray(o)
      ? Object.fromEntries(
          Object.entries(o as Record<string, unknown>)
            .map(([k, v]) => [k, shapeOf(v)])
            .sort(([a], [b]) => (a as string).localeCompare(b as string)),
        )
      : typeof o;

  expect(
    shapeOf(stored),
    "a real sign-in no longer stores what seedSession() fakes — update SEEDED_SHAPE in " +
      "qa/e2e/session.ts, or every spec that seeds a session is testing a shape that no " +
      "longer exists",
  ).toEqual(shapeOf(SEEDED_SHAPE));
});

test("E2E-local-token-storage: the token is sent as x-auth-token and never appears in a URL", async ({ page }) => {
  const urls: string[] = [];
  let sawHeader = false;
  page.on("request", (r) => {
    urls.push(r.url());
    if (r.url().startsWith(`${API}/v1/projects`) && r.headers()["x-auth-token"]) sawHeader = true;
  });
  await page.goto("/sign-in");
  await submit(page, SEEDED.email, SEEDED.password);
  await page.waitForURL(/\/app$/);
  await expect(page.getByTestId(T.sessionBadge)).toBeVisible();

  const token = await page.evaluate(
    () => JSON.parse(window.localStorage.getItem("wheel.session")!).token as string,
  );
  expect(sawHeader).toBe(true);
  // A token in a URL is in history, in Referer, and in every proxy log on the way.
  expect(urls.filter((u) => u.includes(token))).toEqual([]);
});

test("E2E-local-logout: signing out clears the session and the board becomes unreachable", async ({ page }) => {
  await page.goto("/sign-in");
  await submit(page, SEEDED.email, SEEDED.password);
  await page.waitForURL(/\/app$/);

  await page.getByTestId(T.signOut).click();
  await page.waitForURL(/\/sign-in/);
  expect(await page.evaluate(() => window.localStorage.getItem("wheel.session"))).toBeNull();

  // Asserted by navigation, not by a button changing label.
  await page.goto("/app");
  await page.waitForURL(/\/sign-in/);
});

test("E2E-local-revoked: a session the API has revoked signs you out instead of failing silently", async ({ page }) => {
  await page.goto("/sign-in");
  await submit(page, SEEDED.email, SEEDED.password);
  await page.waitForURL(/\/app$/);
  await expect(page.getByTestId(T.sessionBadge)).toBeVisible();

  // Exactly what an expired token looks like from the browser: still stored, no longer accepted.
  await page.evaluate(() => {
    const s = JSON.parse(window.localStorage.getItem("wheel.session")!);
    s.token = "local.00000000-0000-4000-8000-000000000000";
    window.localStorage.setItem("wheel.session", JSON.stringify(s));
  });
  await page.reload();
  await page.waitForURL(/\/sign-in/, { timeout: 15_000 });
  expect(await page.evaluate(() => window.localStorage.getItem("wheel.session"))).toBeNull();
});

test("E2E-local-ratelimit: too many attempts says how long to wait", async ({ page }) => {
  const email = `lockout-${Date.now()}@wheel.dev`;
  await page.goto("/sign-up");
  await submit(page, email, "a-long-enough-password");
  await page.waitForURL(/\/app$/);
  await page.getByTestId(T.signOut).click();
  await page.waitForURL(/\/sign-in/);

  for (let i = 0; i < 5; i += 1) {
    await submit(page, email, "wrong-password-here");
    await expect(page.getByTestId(T.authError)).toBeVisible();
  }
  await submit(page, email, "wrong-password-here");
  await expect(page.getByTestId(T.authError)).toContainText(/\d+ seconds/);
});
