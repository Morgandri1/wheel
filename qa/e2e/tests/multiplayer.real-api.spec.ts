import { test, expect, type Browser, type BrowserContext, type Page } from "@playwright/test";
import { T } from "../testids";

const STUB_HOST = "http://127.0.0.1:8791";
const PASSWORD = "correct-horse-battery";

interface Api {
  status: number;
  body: unknown;
}

/** A call made by the page itself, so the browser sets Origin and the session cookie exactly as the app does. */
async function api(page: Page, method: string, path: string, body?: unknown): Promise<Api> {
  return page.evaluate(
    async ({ method, path, body }) => {
      const res = await fetch(`/api/wheel${path}`, {
        method,
        headers: body === undefined ? {} : { "content-type": "application/json" },
        body: body === undefined ? undefined : JSON.stringify(body),
      });
      const text = await res.text();
      let parsed: unknown = text;
      try {
        parsed = JSON.parse(text);
      } catch {
        /* not JSON */
      }
      return { status: res.status, body: parsed };
    },
    { method, path, body },
  );
}

async function engineHits(page: Page): Promise<{ method: string; path: string }[]> {
  const res = await page.request.get(`${STUB_HOST}/__engine-hits`);
  return res.json();
}

async function clearEngineHits(page: Page) {
  await page.request.delete(`${STUB_HOST}/__engine-hits`);
}

async function signUp(browser: Browser, label: string): Promise<{ ctx: BrowserContext; page: Page; email: string }> {
  const ctx = await browser.newContext();
  const page = await ctx.newPage();
  const email = `${label}-${Date.now()}@example.test`;
  await page.goto("/sign-up", { waitUntil: "domcontentloaded" });
  await page.getByTestId(T.emailInput).fill(email);
  await page.getByTestId(T.passwordInput).fill(PASSWORD);
  await page.getByTestId(T.authSubmit).click({ noWaitAfter: true });
  await expect(page.getByTestId(T.sessionBadge)).toContainText(email, { timeout: 20_000 });
  return { ctx, page, email };
}

async function signIn(page: Page, email: string) {
  await page.goto("/sign-in", { waitUntil: "domcontentloaded" });
  await page.getByTestId(T.emailInput).fill(email);
  await page.getByTestId(T.passwordInput).fill(PASSWORD);
  await page.getByTestId(T.authSubmit).click({ noWaitAfter: true });
  await expect(page.getByTestId(T.sessionBadge)).toContainText(email, { timeout: 20_000 });
}

async function redeem(page: Page, token: string) {
  await page.goto(`/app/invite/${token}`, { waitUntil: "domcontentloaded" });
}

test.describe.serial("E2E-mp: invites and tiers against the real API", () => {
  let browser: Browser;
  let admin: Page;
  let projectId: string;
  let guest: { ctx: BrowserContext; page: Page; email: string };
  let prompter: { ctx: BrowserContext; page: Page; email: string };
  let outsider: { ctx: BrowserContext; page: Page; email: string };
  let guestToken: string;
  let prompterToken: string;

  test.beforeAll(async ({ browser: b }) => {
    browser = b;
    const a = await signUp(browser, "mp-admin");
    admin = a.page;
    const created = await api(admin, "POST", "/v1/projects", { name: `mp-${Date.now().toString(36)}` });
    expect(created.status, JSON.stringify(created.body)).toBe(201);
    projectId = (created.body as { id: string }).id;
    guest = await signUp(browser, "mp-guest");
    prompter = await signUp(browser, "mp-prompter");
    outsider = await signUp(browser, "mp-outsider");
  });

  test.afterAll(async () => {
    for (const c of [admin?.context(), guest?.ctx, prompter?.ctx, outsider?.ctx]) await c?.close();
  });

  async function inviteThroughTheUi(tier: "guest" | "prompter"): Promise<string> {
    await admin.goto(`/app/${projectId}`, { waitUntil: "domcontentloaded" });
    await admin.getByTestId("btn-open-members").click();
    await expect(admin.getByTestId("dialog-members")).toBeVisible();
    await admin.getByTestId("select-invite-tier").selectOption(tier);
    await admin.getByTestId("btn-create-invite").click();
    const token = (await admin.getByTestId("invite-token").textContent())?.trim() ?? "";
    expect(token, "the invite token is shown once").toMatch(/^wi_/);
    return token;
  }

  test("E2E-mp-invite-redeem: an admin's guest invite, redeemed by a second account, lands them on that board", async () => {
    guestToken = await inviteThroughTheUi("guest");
    await redeem(guest.page, guestToken);
    await expect(guest.page).toHaveURL(new RegExp(`/app/${projectId}$`), { timeout: 20_000 });
    await expect(guest.page.getByTestId("btn-open-members")).toBeVisible();

    const me = await api(guest.page, "GET", `/v1/projects/${projectId}`);
    expect(me.status).toBe(200);
    expect((me.body as { tier?: string }).tier, "redeeming lands at the tier the invite named").toBe("guest");
  });

  test("E2E-mp-invite-single-use: the same link cannot be spent twice, and says nothing about why", async () => {
    await redeem(outsider.page, guestToken);
    await expect(outsider.page.getByTestId("invite-error")).toBeVisible({ timeout: 20_000 });
  });

  test("E2E-mp-failed-redeem-keeps-session: a dead invite link does not sign the visitor out", async () => {
    // KNOWN BUG, found by this suite: the API answers an unusable invite with 401 (deliberately one
    // indistinguishable answer) and web's acceptInvite clears the session cookie on any 401 in local
    // mode, so a stale link costs the visitor their login as well as the link. Expected-fail until fixed:
    // when it is fixed this test turns red ("expected to fail, but passed") — delete the annotation then.
    test.fail(true, "BUG: web clears the session cookie when the API answers a dead invite with 401");
    const mine = await api(outsider.page, "GET", "/v1/projects");
    expect(mine.status, "the session is still alive after a dead invite link").toBe(200);
  });

  test("E2E-mp-failed-redeem-grants-nothing: a dead link leaves the visitor a non-member", async () => {
    await signIn(outsider.page, outsider.email);
    const probe = await api(outsider.page, "GET", `/v1/projects/${projectId}`);
    expect(probe.status, "a failed redeem grants nothing").toBe(404);
  });

  test("E2E-mp-prompter-tier: a prompter invite lands at prompter, not guest", async () => {
    prompterToken = await inviteThroughTheUi("prompter");
    await redeem(prompter.page, prompterToken);
    await expect(prompter.page).toHaveURL(new RegExp(`/app/${projectId}$`), { timeout: 20_000 });
    const me = await api(prompter.page, "GET", `/v1/projects/${projectId}`);
    expect((me.body as { tier?: string }).tier).toBe("prompter");
  });

  test("E2E-mp-guest-ui: a guest sees the roster and none of the admin controls", async () => {
    await guest.page.goto(`/app/${projectId}`, { waitUntil: "domcontentloaded" });
    await guest.page.getByTestId("btn-open-members").click();
    await expect(guest.page.getByTestId("member-list")).toBeVisible();
    await expect(guest.page.getByTestId("member-creator")).toBeVisible();
    await expect(guest.page.getByTestId("btn-create-invite")).toHaveCount(0);
    await expect(guest.page.getByTestId("btn-add-member")).toHaveCount(0);
  });

  test("E2E-mp-guest-denied: a guest is refused every admin route, and the host never hears the refused engine calls", async () => {
    await clearEngineHits(admin);
    const p = `/v1/projects/${projectId}`;
    const refused: [string, string, unknown?][] = [
      ["POST", `${p}/invites`, { role: "admin" }],
      ["GET", `${p}/invites`],
      ["POST", `${p}/members`, { user_id: "anyone", role: "admin" }],
      ["PATCH", p, { name: "renamed" }],
      ["DELETE", p],
      ["POST", `${p}/stop`],
      ["POST", `${p}/builder/turns`, { message: "hi" }],
      ["POST", `${p}/engine/v1/nodes`, { name: "x", type: "ctx", config: { markdown: "" } }],
      ["POST", `${p}/engine/v1/agents/00000000-0000-0000-0000-000000000000/send`, { body: "hi" }],
    ];
    for (const [method, path, body] of refused) {
      const r = await api(guest.page, method, path, body);
      expect(r.status, `guest ${method} ${path} -> ${JSON.stringify(r.body)}`).toBe(403);
    }
    expect(await engineHits(admin), "no refused call reached the host").toEqual([]);

    const board = await api(guest.page, "GET", `${p}/engine/v1/board`);
    expect(board.status, "a guest may read the board").toBe(200);
    expect(await engineHits(admin)).toEqual([expect.objectContaining({ method: "GET", path: "/v1/board" })]);
  });

  test("E2E-mp-prompter-bounds: a prompter may message an agent but not build, invite, or manage", async () => {
    await clearEngineHits(admin);
    const p = `/v1/projects/${projectId}`;
    for (const [method, path, body] of [
      ["POST", `${p}/invites`, { role: "guest" }],
      ["POST", `${p}/builder/turns`, { message: "hi" }],
      ["POST", `${p}/engine/v1/nodes`, { name: "x", type: "ctx", config: { markdown: "" } }],
      ["DELETE", p, undefined],
    ] as [string, string, unknown][]) {
      const r = await api(prompter.page, method, path, body);
      expect(r.status, `prompter ${method} ${path} -> ${JSON.stringify(r.body)}`).toBe(403);
    }
    expect(await engineHits(admin)).toEqual([]);

    const send = await api(prompter.page, "POST", `${p}/engine/v1/agents/00000000-0000-0000-0000-000000000000/send`, {
      body: "hi",
    });
    expect(send.status, "a prompter may prompt").toBe(200);
    expect(await engineHits(admin)).toEqual([expect.objectContaining({ method: "POST" })]);
  });

  test("E2E-mp-nonmember: an account with no membership cannot tell the project exists", async () => {
    const p = `/v1/projects/${projectId}`;
    for (const [method, path, body] of [
      ["GET", p, undefined],
      ["GET", `${p}/members`, undefined],
      ["POST", `${p}/invites`, { role: "guest" }],
      ["GET", `${p}/engine/v1/board`, undefined],
    ] as [string, string, unknown][]) {
      const r = await api(outsider.page, method, path, body);
      expect(r.status, `outsider ${method} ${path}`).toBe(404);
    }
  });

  test("E2E-mp-admin-still-admin: the creator keeps admin after others join", async () => {
    const me = await api(admin, "GET", `/v1/projects/${projectId}`);
    expect((me.body as { tier?: string }).tier).toBe("admin");
    const invites = await api(admin, "GET", `/v1/projects/${projectId}/invites`);
    expect(invites.status).toBe(200);
  });
});
