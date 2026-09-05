import { test, expect } from "@playwright/test";
import { T } from "../testids";

/**
 * E2E-vault-masked — S1.
 *
 * "Masked" must mean the value never reaches the browser, not that it is hidden with CSS
 * or a password input. So this asserts on the DOM *and* on every response body the page
 * received. A value present in a network response is disclosed to anyone with devtools,
 * however the UI chooses to render it.
 */
const SECRET = "canary-vault-value-9c1f7ae2";

test("E2E-vault-masked: a written vault value never comes back to the browser", async ({ page }) => {
  const leaks: string[] = [];
  page.on("response", async (res) => {
    try {
      const body = await res.text();
      if (body.includes(SECRET)) leaks.push(`${res.status()} ${res.url()}`);
    } catch {
      /* non-text bodies cannot carry the canary in a readable form */
    }
  });

  await page.goto("/app");
  await page.getByTestId(T.paletteNode("vault")).click();
  await page.getByTestId(T.node("secrets")).click();
  await page.getByTestId(T.inspectorField("keys")).fill("API_KEY");
  await page.getByTestId(T.inspectorSave).click();

  await page.getByTestId(T.vaultValueInput).fill(SECRET);
  await page.getByTestId(T.inspectorSave).click();

  await page.reload();
  await page.getByTestId(T.node("secrets")).click();

  // The key NAME is expected to be visible; the value must not be, anywhere.
  await expect(page.getByTestId(T.vaultKeyRow("API_KEY"))).toBeVisible();
  expect(await page.content(), "vault value rendered into the DOM").not.toContain(SECRET);
  expect(leaks, `vault value present in response bodies: ${leaks.join(", ")}`).toEqual([]);
});
