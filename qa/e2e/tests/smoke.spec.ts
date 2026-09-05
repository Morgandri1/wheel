import { test, expect } from "@playwright/test";
import { T } from "../testids";

/**
 * M1 vertical-slice smoke — TESTPLAN E2E-*.
 *
 * Mirrors the milestone: create a project, place an agent and a ctx node, wire ctx->agent,
 * start the agent against the fake harness, send a chat message, see the reply in the log.
 *
 * The assertions lean on the fake harness echoing what it received, so E2E-injection-visible
 * can prove through the UI that the ctx markdown genuinely reached the child — not merely
 * that the UI drew a wire between two boxes.
 */

const CTX_CANARY = "the-sky-is-green-4f2a";

test.describe("M1 vertical slice", () => {
  test("E2E-landing: landing renders with no console errors", async ({ page }) => {
    const errors: string[] = [];
    page.on("console", (m) => m.type() === "error" && errors.push(m.text()));
    page.on("pageerror", (e) => errors.push(String(e)));

    await page.goto("/");
    await expect(page.getByTestId(T.landingHero)).toBeVisible();
    expect(errors, `console errors on landing: ${errors.join(" | ")}`).toEqual([]);
  });

  test("E2E-signin: unauthenticated /app does not render the board", async ({ page }) => {
    await page.goto("/app");
    // Either redirected away, or shown a sign-in affordance — but never the board itself.
    await expect(page.getByTestId(T.board)).toHaveCount(0);
  });

  test("E2E-project-create → chat: the whole slice", async ({ page }) => {
    await page.goto("/app");

    await page.getByTestId(T.projectNew).click();
    await page.getByTestId(T.projectNameInput).fill("e2e-slice");
    await page.getByTestId(T.projectCreateSubmit).click();
    await expect(page.getByTestId(T.projectStatus)).toContainText(/running|starting/i);

    // E2E-place-nodes
    await page.getByTestId(T.paletteNode("agent")).click();
    await page.getByTestId(T.paletteNode("ctx")).click();
    await expect(page.getByTestId(T.node("researcher"))).toBeVisible();
    await expect(page.getByTestId(T.node("house-style"))).toBeVisible();

    // persistence across reload — placing a node that vanishes on refresh is a
    // client-side illusion, and the board is meant to be durable server state.
    await page.reload();
    await expect(page.getByTestId(T.node("researcher"))).toBeVisible();

    // E2E-inspector: put the canary into the ctx node
    await page.getByTestId(T.node("house-style")).click();
    await expect(page.getByTestId(T.inspector)).toBeVisible();
    await page.getByTestId(T.inspectorField("markdown")).fill(`# House style\n\n${CTX_CANARY}\n`);
    await page.getByTestId(T.inspectorSave).click();

    // E2E-wire: ctx -> agent (send) is the injection wire
    await page.getByTestId(T.node("house-style")).dragTo(page.getByTestId(T.node("researcher")));
    await expect(page.getByTestId(T.wire("house-style", "researcher", "send"))).toBeVisible();

    // E2E-start-agent (fake harness)
    await page.getByTestId(T.node("researcher")).click();
    await page.getByTestId(T.agentStart).click();
    await expect(page.getByTestId(T.nodeStatus("researcher"))).toContainText(/running|idle/i, {
      timeout: 30_000,
    });

    // E2E-chat + E2E-injection-visible. The fake echoes what it was given, so the canary
    // appearing in the log proves the ctx markdown reached the child's prompt — the UI
    // drawing a wire proves only that the UI drew a wire.
    await page.getByTestId(T.chatInput).fill("hello from e2e");
    await page.getByTestId(T.chatSend).click();
    await expect(page.getByTestId(T.agentLog)).toContainText("hello from e2e", { timeout: 30_000 });
    await expect(page.getByTestId(T.agentLog)).toContainText(CTX_CANARY, { timeout: 30_000 });
  });

  test("E2E-wire-illegal: an illegal wire is refused in the UI, with a reason", async ({ page }) => {
    await page.goto("/app");
    await page.getByTestId(T.paletteNode("table")).click();
    await page.getByTestId(T.paletteNode("vault")).click();

    // table -> vault is denied by the matrix in every wire type.
    await page.getByTestId(T.node("findings")).dragTo(page.getByTestId(T.node("secrets")));

    await expect(page.getByTestId(T.wireError)).toBeVisible();
    await expect(page.getByTestId(T.wire("findings", "secrets", "read"))).toHaveCount(0);
  });
});
