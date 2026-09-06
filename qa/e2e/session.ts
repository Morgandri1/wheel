import type { Page } from "@playwright/test";

/**
 * Put a signed-in session in place without driving the sign-in form.
 *
 * Web's suggestion, and the reason for it is the good part: with every downstream spec
 * typing into the form, a broken form fails all of them at once, and a downstream green
 * tells you nothing about its own subject. Seeding keeps local-auth.spec.ts as the single
 * place a sign-in failure can come from.
 *
 * THE HAZARD THIS BRINGS, and why SEEDED_SHAPE exists. A seeded session is a fake of the
 * app's own state. If the real shape changes — a renamed field, a nested user, a second
 * token — every seeded spec goes on passing against a shape the app no longer produces,
 * and they are all testing a fiction. That is the same failure as a mock nobody checks
 * against the thing it mocks.
 *
 * So `E2E-local-session-shape` in local-auth.spec.ts signs in FOR REAL and asserts what
 * lands in localStorage matches this shape. Seed freely; the guard is what makes it safe.
 *
 * Verified: the guard passes against a real sign-in (14.1s), and its shape comparison
 * rejects both drift shapes that matter — a seed that gains a field the app does not store,
 * and a renamed token. Keys and types are compared, never values, since the token and id
 * differ every session.
 */
export const SESSION_KEY = "wheel.session";

export const SEEDED_SHAPE = {
  token: "local.00000000-0000-4000-8000-0000000000ff",
  user: { id: "usr_e2e_seeded", email: "seeded@example.test" },
};

export type Session = typeof SEEDED_SHAPE;

/**
 * Runs before any of the page's own scripts, so the app reads the session on first paint
 * rather than briefly rendering signed-out and settling. addInitScript survives navigation,
 * which page.evaluate does not.
 */
export async function seedSession(page: Page, overrides: Partial<Session> = {}) {
  const session = { ...SEEDED_SHAPE, ...overrides };
  await page.addInitScript(
    ([key, value]) => {
      try {
        window.localStorage.setItem(key as string, value as string);
      } catch {
        // A browser with site data blocked throws here. Swallowing keeps the failure where
        // it belongs — the assertion that the page is signed in — instead of a stack trace
        // from setup that says nothing about what was being tested.
      }
    },
    [SESSION_KEY, JSON.stringify(session)] as const,
  );
  return session;
}
