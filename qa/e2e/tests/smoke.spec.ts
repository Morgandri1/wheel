import { test, expect } from "@playwright/test";
import { T } from "../testids";
import { addNode, addWire, board, createProject, deleteProject, startProject, tryWire } from "../api";

/**
 * M1 vertical slice — TESTPLAN E2E-*.
 *
 * The board is built over the API and the browser is used only for what only a browser
 * can check. Web asked for this and they are right: twenty setup clicks before the
 * assertion starts is how E2E suites become slow and flaky, and a failure in the setup
 * clicks reads as a failure of whatever the test was actually about.
 *
 * Node testids are keyed by UUID (`node-<id>`), not by name, so ids come back from the
 * API rather than being guessed.
 */

const CTX_CANARY = "the-sky-is-green-4f2a";

test.describe("M1 vertical slice", () => {
  test("E2E-landing: landing renders with no console errors", async ({ page }) => {
    const errors: string[] = [];
    page.on("console", (m) => m.type() === "error" && errors.push(m.text()));
    page.on("pageerror", (e) => errors.push(String(e)));

    await page.goto("/");
    await expect(page.getByTestId(T.ctaApp)).toBeVisible();

    // BUG-007 (Web, S3, open): WheelMark computes SVG spoke coordinates with Math.cos/Math.sin
    // and the last digit differs between the Node renderer and browser V8, so React reports a
    // hydration mismatch. It is INTERMITTENT — whether the two engines round identically varies
    // per platform and run — which is why this is a targeted allowlist rather than test.fail():
    // an expected-failure annotation goes red on the runs where the bug does not reproduce, and
    // a skip would stop checking the page altogether. Every OTHER console error still fails.
    const known = (e: string) => /hydrat|hydration-mismatch/i.test(e);
    const unexpected = errors.filter((e) => !known(e));
    expect(unexpected, `console errors on landing:\n${unexpected.join("\n")}`).toEqual([]);
    if (errors.some(known)) console.log("note: BUG-007 hydration mismatch reproduced this run");
  });

  test("E2E-signin: the landing CTA reaches the projects list", async ({ page }) => {
    await page.goto("/");
    await page.getByTestId(T.ctaApp).click();
    await expect(page).toHaveURL(/\/app/);
    await expect(
      page.getByTestId(T.projectList).or(page.getByTestId(T.projectNewEmpty)),
    ).toBeVisible();
  });

  test("E2E-place-nodes + E2E-inspector: the board renders server state", async ({ page }) => {
    // This test starts a real sandbox. Per §4b the host blocks on /start until the engine's
    // healthz is green, up to 30s, so the default 30s budget is spent before the assertions
    // begin. Raised deliberately rather than trimming the test: starting the project IS the
    // precondition for the board rendering server state.
    test.setTimeout(120_000);

    const project = await createProject(`e2e-slice-${Date.now().toString(36)}`);
    try {
      const ctx = await addNode(project.id, {
        name: "house-style",
        type: "ctx",
        config: { markdown: `# House style\n\n${CTX_CANARY}\n` },
      });
      const agent = await addNode(project.id, {
        name: "researcher",
        type: "agent",
        config: {
          harness: "claude",
          system_prompt: "You research things.",
          run_on_startup: false,
          ephemeral_context: false,
        },
      });
      // ctx -> agent (send) is the injection wire.
      await addWire(project.id, ctx.id, agent.id, "send");

      await startProject(project.id);
      await page.goto(`/app/${project.id}`);
      await expect(page.getByTestId(T.board)).toBeVisible();
      await expect(page.getByTestId(T.node("house-style"))).toBeVisible();
      await expect(page.getByTestId(T.node("researcher"))).toBeVisible();

      // Durable server state, not a client-side illusion.
      await page.reload();
      await expect(page.getByTestId(T.node("researcher"))).toBeVisible();

      // E2E-inspector: selecting the ctx node shows its markdown, canary included.
      await page.getByTestId(T.node("house-style")).click();
      await expect(page.getByTestId(T.inspectorEmpty)).toHaveCount(0);
      await expect(page.getByTestId(T.inspectorCtxMarkdown)).toHaveValue(
        new RegExp(CTX_CANARY),
      );
    } finally {
      await deleteProject(project.id);
    }
  });

  test("E2E-wire-illegal: a denied wire is refused by the engine, not just the UI", async ({}) => {
    const project = await createProject(`e2e-wire-${Date.now().toString(36)}`);
    try {
      const table = await addNode(project.id, {
        name: "findings",
        type: "table",
        config: { columns: [{ name: "claim", type: "text" }] },
      });
      const vault = await addNode(project.id, {
        name: "secrets",
        type: "vault",
        config: { keys: ["API_KEY"] },
      });

      // table -> vault is denied by the §3 matrix in every wire type. The UI refusing to
      // offer it is not the assertion that matters; a client can always be bypassed, so
      // this asserts the server refuses it.
      for (const type of ["read", "write", "send"]) {
        const res = await tryWire(project.id, table.id, vault.id, type);
        expect(res.status, `table->vault (${type}) was accepted: ${res.body}`).toBeGreaterThanOrEqual(400);
      }

      const after = await board(project.id);
      const wires = after.nodes.flatMap((n) => n.wires ?? []);
      expect(wires, "a denied wire was persisted anyway").toEqual([]);
    } finally {
      await deleteProject(project.id);
    }
  });
});
