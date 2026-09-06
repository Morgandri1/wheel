import { expect, type Locator, type Page } from "@playwright/test";

/**
 * Prove the client bundle actually hydrated, rather than that the server sent HTML.
 *
 * Web caught a green check asserting "renders" on a page whose bundle never hydrated, and
 * this is the reason it could: with JavaScript DISABLED, the landing page's CTA is still
 * `toBeVisible()`, still has text, still has its stylesheet. Every presence-only assertion
 * passes against a page that does nothing at all. `toBeEnabled()` is no better — SSR emits
 * enabled controls — and I had used exactly that in my own packaged spec.
 *
 * Measured on the real bundle: with JS on, the CTA element carries 2 `__react*` keys; with
 * JS off, zero, while remaining perfectly visible. React attaches those when it hydrates, so
 * their presence is the difference between "the browser received markup" and "the app is
 * running". It is an internal of React rather than a public API — if a future version stops
 * setting them this helper fails loudly and everything using it goes red, which is the right
 * direction for a check whose whole job is to not be fooled.
 */
export async function expectHydrated(target: Locator, what = "this element") {
  await expect(target).toBeVisible();
  await expect
    .poll(
      async () =>
        target.evaluate((el) => Object.keys(el).filter((k) => k.startsWith("__react")).length),
      {
        timeout: 15_000,
        message:
          `${what} is in the DOM but React never attached to it. The server sent HTML and ` +
          `the client bundle did not take over: the page LOOKS right and responds to ` +
          `nothing. toBeVisible() cannot tell these apart — with JS disabled it still passes.`,
      },
    )
    .toBeGreaterThan(0);
}

/** The same claim for a page as a whole, via whichever element is its real entry point. */
export async function expectPageHydrated(page: Page, testId: string) {
  await expectHydrated(page.getByTestId(testId), `[data-testid="${testId}"]`);
}
