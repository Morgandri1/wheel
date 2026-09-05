import { test, expect } from "@playwright/test";
import { T } from "../testids";
import { addNode, createProject, deleteProject, putSecret } from "../api";

/**
 * E2E-vault-masked — S1.
 *
 * "Masked" must mean the value never reaches the browser, not that it is hidden behind
 * CSS or a password input. A value present in a network response is disclosed to anyone
 * with devtools, an extension, or an XSS, however the UI chooses to render it — so this
 * asserts on every response body the page received, and on the DOM.
 *
 * The secret is written over the API rather than through the inspector: Web ships no
 * testid for a vault value field (correctly — §3 makes vault values write-only through
 * the API), so driving it through the UI would be testing a control that should not
 * exist.
 */
const SECRET = "canary-vault-value-9c1f7ae2";

test("E2E-vault-masked: a written vault value never comes back to the browser", async ({ page }) => {
  const project = await createProject(`e2e-vault-${Date.now().toString(36)}`);
  const leaks: string[] = [];

  page.on("response", async (res) => {
    const ct = res.headers()["content-type"] ?? "";
    if (!/json|text|javascript|html/.test(ct)) return;
    try {
      if ((await res.text()).includes(SECRET)) leaks.push(`${res.status()} ${res.url()}`);
    } catch {
      /* body consumed or navigation raced — not evidence of a leak */
    }
  });

  try {
    const vault = await addNode(project.id, {
      name: "secrets",
      type: "vault",
      config: { keys: ["API_KEY"] },
    });

    const status = await putSecret(project.id, vault.id, "API_KEY", SECRET);
    expect(status, "vault write was rejected, so the masking assertion would be vacuous")
      .toBeLessThan(300);

    await page.goto(`/app/${project.id}`);
    await expect(page.getByTestId(T.board)).toBeVisible();
    await page.getByTestId(T.node(vault.id)).click();

    // The key NAME is expected to be visible; the value must not be, anywhere.
    await expect(page.getByTestId(T.inspectorEmpty)).toHaveCount(0);
    expect(await page.content(), "vault value rendered into the DOM").not.toContain(SECRET);
    expect(leaks, `vault value present in response bodies:\n${leaks.join("\n")}`).toEqual([]);
  } finally {
    await deleteProject(project.id);
  }
});
