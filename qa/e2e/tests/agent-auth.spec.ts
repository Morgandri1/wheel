import { test, expect } from "@playwright/test";
import { createProject, addNode, startProject, deleteProject } from "../api";
import { T } from "../testids";
import { expectHydrated } from "../hydration";

const KEY = "sk-test-canary-9f3a-never-echoed";

// This spec runs first alphabetically and deliberately absorbs `next dev`'s on-demand
// compilation of BOTH /app and /app/[projectId] — roughly 25s cold — so that the specs after it
// are timed against a warm server instead of paying a bill they did not incur. The systemic fix
// is to run E2E against `next build && next start`, where nothing compiles on demand; until the
// harness does that, this one test carries it and says so.
test.setTimeout(120_000);

test("agent api-key auth: needs_auth -> authenticate -> authenticated, key never echoed", async ({ page }) => {
  // Warm /app before anything is timed. `next dev` compiles routes on demand — /app takes ~11s
  // cold — and whichever test navigates there first pays for it. Paying it here means this spec
  // does not quietly hand the bill to whatever runs next.
  await page.goto("/app");

  const project = await createProject(`auth-${Date.now().toString(36)}`);
  try {
    const agent = await addNode(project.id, {
      name: "planner",
      type: "agent",
      config: { harness: "claude", system_prompt: "", run_on_startup: false, ephemeral_context: false },
    });
    await startProject(project.id);

    // Starting an unauthenticated agent must land in needs_auth, not running.
    await fetch(`http://localhost:8787/v1/projects/${project.id}/engine/v1/agents/${agent.id}/start`, {
      method: "POST",
      headers: { "x-auth-token": "e2e", "x-project-id": project.id },
    });

    await page.goto(`/app/${project.id}`);
    const status = page.getByTestId("node-planner-status");
    await expect(status).toHaveAttribute("data-status", "needs_auth");

    // The plate offers the fix, not just the diagnosis.
    await page.getByTestId("node-planner-authenticate").click();
    // The callout is not decoration: it carries the button that fixes the problem it
    // reports. Visible-but-dead would tell the operator their agent can be authenticated
    // here and then do nothing when they try.
    await expectHydrated(page.getByTestId("auth-needs-auth-callout"), "the needs_auth callout");

    const field = page.getByTestId("input-api-key");
    await expect(field).toHaveAttribute("type", "password");
    await field.fill(KEY);
    await page.getByTestId("btn-auth-complete").click();

    await expect(page.getByTestId("auth-status")).toHaveAttribute("data-authenticated", "true");

    // The key must not survive anywhere the browser can see it.
    await expect(page.getByTestId("input-api-key")).toHaveCount(0);
    const dom = await page.content();
    expect(dom.includes(KEY), "api key leaked into the DOM").toBe(false);

    // Authenticating does not start the agent: the person still has to restart it.
    await expect(status).not.toHaveAttribute("data-status", "running");
  } finally {
    await deleteProject(project.id);
  }
});
