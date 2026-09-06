import { test, expect } from "@playwright/test";
import { addNode, addWire, createProject, deleteProject, putSecret, startProject } from "../api";
import { T } from "../testids";

/**
 * E2E-auth-first-paint · E2E-auth-vault-reachable.
 *
 * The operator's report was "when I open an agent, the 'sign in with Anthropic' dialog opens for
 * a moment before disappearing". A flash is invisible to an assertion that runs after it, so this
 * does not poll for the form — it records every mount and unmount of it and asserts on the
 * sequence afterwards. `/auth` is deliberately slowed so the window a fast local mock would hide
 * is wide open.
 *
 * The second half guards the other direction: a credential that comes from a vault must not make
 * the browser sign-in unreachable. That is how a vault gets its FIRST value.
 */
const API = process.env.NEXT_PUBLIC_API_URL ?? "http://localhost:8787";
const TOKEN = process.env.WHEEL_E2E_TOKEN ?? "dev";

type Recorder = { __authFlowEvents: string[] };

async function recordSignInMounts(page: import("@playwright/test").Page) {
  await page.addInitScript(() => {
    const w = window as unknown as { __authFlowEvents: string[] };
    w.__authFlowEvents = [];
    const SEL = '[data-testid="auth-flow"]';
    const hits = (n: Node) => n instanceof Element && (n.matches(SEL) || !!n.querySelector(SEL));
    const observe = () =>
      new MutationObserver((records) => {
        for (const r of records) {
          r.addedNodes.forEach((n) => hits(n) && w.__authFlowEvents.push("mount"));
          r.removedNodes.forEach((n) => hits(n) && w.__authFlowEvents.push("unmount"));
        }
      }).observe(document.documentElement, { childList: true, subtree: true });
    if (document.documentElement) observe();
    else document.addEventListener("DOMContentLoaded", observe);
  });
}

/** Hold the credential check open, so the pre-answer paint is observable rather than theoretical. */
async function slowAuthStatus(page: import("@playwright/test").Page, ms: number) {
  await page.route("**/engine/v1/agents/*/auth", async (route) => {
    if (route.request().method() !== "GET") return route.continue();
    await new Promise((r) => setTimeout(r, ms));
    await route.continue();
  });
}

test("E2E-auth-first-paint: an authenticated agent never flashes the sign-in form", async ({ page }) => {
  test.setTimeout(90_000);
  const project = await createProject(`paint-${Date.now().toString(36)}`);
  try {
    const agent = await addNode(project.id, {
      name: "planner",
      type: "agent",
      config: { harness: "claude", system_prompt: "", run_on_startup: false, ephemeral_context: false },
    });
    await startProject(project.id);

    // Give the agent a credential, so /auth answers `authenticated: true` — the exact case that
    // flashed: the form was painted from `?? false` and swapped out when the answer arrived.
    const stored = await fetch(
      `${API}/v1/projects/${project.id}/engine/v1/agents/${agent.id}/auth/complete`,
      {
        method: "POST",
        headers: { "x-auth-token": TOKEN, "x-project-id": project.id, "content-type": "application/json" },
        body: JSON.stringify({ api_key: "sk-ant-first-paint" }),
      },
    );
    expect(stored.status, "the agent was not authenticated, so this test would prove nothing")
      .toBeLessThan(300);

    await recordSignInMounts(page);
    await slowAuthStatus(page, 2000);

    await page.goto(`/app/${project.id}`);
    await page.getByTestId(T.node("planner")).click();

    // While the answer is outstanding: a placeholder, and nothing that claims either state.
    await expect(page.getByTestId(T.authPending)).toBeVisible();
    await expect(page.getByTestId(T.authFlow)).toHaveCount(0);
    await expect(page.getByTestId(T.authStatus)).toHaveCount(0);

    await expect(page.getByTestId(T.authStatus)).toHaveAttribute("data-authenticated", "true");

    const events = await page.evaluate(() => (window as unknown as Recorder).__authFlowEvents);
    expect(events, "the sign-in form was mounted and then taken away — that is the flash").toEqual([]);
  } finally {
    await deleteProject(project.id);
  }
});

test("E2E-auth-vault-reachable: a vault-provided credential still exposes a way to sign in", async ({ page }) => {
  test.setTimeout(90_000);
  const project = await createProject(`vaultauth-${Date.now().toString(36)}`);
  try {
    const agent = await addNode(project.id, {
      name: "planner",
      type: "agent",
      config: { harness: "claude", system_prompt: "", run_on_startup: false, ephemeral_context: false },
    });
    const vault = await addNode(project.id, {
      name: "anthropic-team",
      type: "vault",
      config: { keys: ["ANTHROPIC_API_KEY"] },
    });
    await addWire(project.id, agent.id, vault.id, "read");
    await putSecret(project.id, vault.id, "ANTHROPIC_API_KEY", "sk-ant-from-a-vault");
    await startProject(project.id);

    await page.goto(`/app/${project.id}`);
    await page.getByTestId(T.node("planner")).click();

    const chip = page.getByTestId(T.authStatus);
    await expect(chip).toHaveAttribute("data-mode", "env");
    await expect(chip).toContainText("anthropic-team");

    // Without this the browser OAuth flow is unreachable on exactly the boards that need it.
    await page.getByTestId(T.authDifferentAccount).click();
    await page.getByTestId(T.authOauth).click();

    // The share target only exists once a sign-in is open, and it must already point at the vault
    // the credential came from: signing in again means replacing that value, not shadowing it with
    // a private copy that leaves every other agent on the old one.
    await expect(page.getByTestId(T.authPasteCode)).toBeVisible();
    await expect(page.getByTestId(T.authVaultShare)).toHaveValue("anthropic-team");
  } finally {
    await deleteProject(project.id);
  }
});
