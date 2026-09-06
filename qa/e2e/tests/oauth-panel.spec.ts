import { test, expect } from "@playwright/test";
import { createProject, addNode, startProject, deleteProject } from "../api";
import { T } from "../testids";

/**
 * E2E-oauth-* — the paste-code sign-in panel (§4 auth/begin | auth/complete).
 *
 * Owed by qa:testid-parity: auth-link, auth-user-code and input-auth-code sat in the
 * DEFERRED block while Web shipped M1 as API-key-only, and the gate went red the moment
 * components/inspector/oauth-panel.tsx started rendering them. A deferred selector that
 * works is a test somebody owes; this is that test.
 *
 * auth/begin is stubbed at the network boundary rather than driven through a real engine.
 * The engine's own side of this flow already has 15 assertions in
 * qa/integration/test_engine_auth_paste.py (AUTH-paste-*), including that an abandoned
 * login is reaped by TTL. What is unproven, and what a browser is the only way to prove,
 * is that the panel renders what the engine returned VERBATIM and sends back exactly what
 * the user typed. Stubbing lets this assert the expiry and device-code shapes too, which
 * a live engine will not produce on demand.
 */

const BEGUN = {
  mode: "paste_code" as const,
  url: "https://claude.ai/oauth/authorize?code=challenge-9f3a",
  user_code: "WDJB-MJHT",
  instructions: "Open the link, approve, then paste the code it shows you.",
  session: "sess-e2e-1",
  expires_in: 600,
};

// `next dev` compiles /app/[projectId] on demand — tens of seconds cold — and whichever
// spec reaches it first pays for it. agent-auth.spec.ts absorbs that when the whole suite
// runs alphabetically, but this file must also work when run ALONE, which is exactly how
// anybody debugging it will run it. A spec that only passes as part of an ordered suite
// fails for the person trying to understand why it failed.
test.setTimeout(120_000);

async function openAgentInspector(page: import("@playwright/test").Page, projectId: string) {
  await page.goto(`/app/${projectId}`);
  // Clicking the NODE opens the inspector unconditionally. The `node-planner-authenticate`
  // affordance only exists once the agent is in needs_auth, so reaching for it here made
  // the test hang on a locator that would never appear — and the hang looked like the
  // OAuth panel being broken rather than the test asking for the wrong thing.
  await page.getByTestId(T.node("planner")).click();
}

test.describe("oauth paste-code panel", () => {
  let projectId = "";

  test.beforeEach(async () => {
    const project = await createProject(`oauth-${Date.now().toString(36)}`);
    projectId = project.id;
    await addNode(projectId, {
      name: "planner",
      type: "agent",
      config: { harness: "claude", system_prompt: "", run_on_startup: false, ephemeral_context: false },
    });
    await startProject(projectId);
  });

  test.afterEach(async () => {
    if (projectId) await deleteProject(projectId);
  });

  test("E2E-oauth-begin: the engine's url, code and instructions are shown verbatim", async ({ page }) => {
    await page.route("**/auth/begin", (route) =>
      route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify(BEGUN) }),
    );

    await openAgentInspector(page, projectId);
    await page.getByTestId(T.authOauth).click();

    // Verbatim, not merely "a link exists". The engine owns this URL; a panel that
    // rebuilds or normalises it would send the user to a login that does not match the
    // session, and the failure would look like the user mistyping their code.
    const link = page.getByTestId(T.authLink);
    await expect(link).toHaveAttribute("href", BEGUN.url);
    await expect(link).toHaveText(BEGUN.url);

    // A credential page must open in a new tab and must not hand the opener a window
    // reference back — noopener is load-bearing here, not decoration.
    await expect(link).toHaveAttribute("target", "_blank");
    await expect(link).toHaveAttribute("rel", /noopener/);

    await expect(page.getByTestId(T.authUserCode)).toHaveText(BEGUN.user_code);
    await expect(page.getByTestId(T.authPasteCode)).toContainText(BEGUN.instructions);

    // The box the user pastes into starts EMPTY. A pre-filled code would be the panel
    // guessing on the user's behalf at the one step where guessing is indistinguishable
    // from a phishing prompt.
    await expect(page.getByTestId(T.authCodeInput)).toHaveValue("");
  });

  test("E2E-oauth-complete: the typed code is sent, with the session that issued it", async ({ page }) => {
    await page.route("**/auth/begin", (route) =>
      route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify(BEGUN) }),
    );

    let sent: Record<string, unknown> | null = null;
    await page.route("**/auth/complete", async (route) => {
      sent = JSON.parse(route.request().postData() ?? "{}");
      await route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify({ authenticated: true, account: "e2e@example.test" }),
      });
    });

    await openAgentInspector(page, projectId);
    await page.getByTestId(T.authOauth).click();
    await page.getByTestId(T.authCodeInput).fill("PASTED-CODE-7f3a");
    await page.getByTestId(T.authCodeInput).press("Enter");

    await expect.poll(() => sent).not.toBeNull();
    // Exactly what was typed, and tied to the begin that issued it. Dropping `session`
    // is the bug that makes a stale code from an earlier attempt succeed.
    expect(sent!.code).toBe("PASTED-CODE-7f3a");
    expect(sent!.session).toBe(BEGUN.session);
  });

  test("E2E-oauth-expiry: a closed window disables the box and says to start again", async ({ page }) => {
    // One second, so the countdown genuinely elapses rather than being simulated.
    await page.route("**/auth/begin", (route) =>
      route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify({ ...BEGUN, expires_in: 1 }),
      }),
    );

    await openAgentInspector(page, projectId);
    await page.getByTestId(T.authOauth).click();

    const input = page.getByTestId(T.authCodeInput);
    await expect(input).toBeEnabled();
    // An expired sign-in cannot be retyped out of. If the box stayed live, the user would
    // paste a valid-looking code, be told it was wrong, and blame themselves.
    await expect(input).toBeDisabled({ timeout: 15_000 });
    await expect(page.getByTestId(T.authPasteCode)).toContainText(/start the sign-in again/i);
  });
});
