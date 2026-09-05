import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, render, screen } from "@testing-library/react";

/**
 * The session store is the only thing standing between "the API said your token is dead" and a
 * board that keeps firing doomed requests, so the tests that matter here are the forgetting ones.
 *
 * Each case imports the module fresh: it holds process-wide state on purpose (one session per
 * browser) and a leaked session between cases would make an assertion pass for the wrong reason.
 */

type Mod = typeof import("./local-auth");

async function load(mode = "local"): Promise<Mod> {
  vi.stubEnv("NEXT_PUBLIC_AUTH_MODE", mode);
  vi.resetModules();
  return (await import("./local-auth")) as Mod;
}

function respond(status: number, body: unknown, headers: Record<string, string> = {}) {
  return {
    ok: status >= 200 && status < 300,
    status,
    headers: { get: (k: string) => headers[k.toLowerCase()] ?? null },
    json: async () => body,
  } as unknown as Response;
}

const SESSION = { token: "local.abc", user: { id: "u1", email: "dev@wheel.dev" } };

beforeEach(() => {
  window.localStorage.clear();
});

afterEach(() => {
  vi.unstubAllEnvs();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("hydration", () => {
  it("starts in loading, so a gate never mistakes 'not looked yet' for 'signed out'", async () => {
    const m = await load();
    expect(m.useSession).toBeTypeOf("function");
    // The snapshot before hydrateSession() is the one a gate reads on first render.
    expect(m.sessionToken()).toBeNull();
  });

  it("restores a stored session", async () => {
    window.localStorage.setItem("wheel.session", JSON.stringify(SESSION));
    const m = await load();
    m.hydrateSession();
    expect(m.sessionToken()).toBe("local.abc");
  });

  it.each([
    ["not json at all", "{{{"],
    ["a session with no token", JSON.stringify({ user: SESSION.user })],
    ["a session with no user", JSON.stringify({ token: "local.abc" })],
    ["a user missing its email", JSON.stringify({ token: "local.abc", user: { id: "u1" } })],
  ])("treats %s as signed out rather than throwing", async (_label, raw) => {
    window.localStorage.setItem("wheel.session", raw);
    const m = await load();
    expect(() => m.hydrateSession()).not.toThrow();
    expect(m.sessionToken()).toBeNull();
  });
});

describe("signing in", () => {
  it("keeps the token and mirrors it to storage", async () => {
    const m = await load();
    vi.stubGlobal("fetch", vi.fn(async () => respond(200, SESSION)));
    const user = await m.signIn("dev@wheel.dev", "wheel-dev-password");
    expect(user.email).toBe("dev@wheel.dev");
    expect(m.sessionToken()).toBe("local.abc");
    expect(JSON.parse(window.localStorage.getItem("wheel.session")!).token).toBe("local.abc");
  });

  it("refuses a response it cannot read instead of storing a broken session", async () => {
    const m = await load();
    vi.stubGlobal("fetch", vi.fn(async () => respond(200, { token: 42 })));
    await expect(m.signIn("dev@wheel.dev", "x")).rejects.toThrow(/can't read/i);
    expect(m.sessionToken()).toBeNull();
  });

  it("surfaces the API's own message for a rejected credential", async () => {
    const m = await load();
    vi.stubGlobal(
      "fetch",
      vi.fn(async () =>
        respond(401, { error: { code: "invalid_credentials", message: "that email and password don't match" } }),
      ),
    );
    await expect(m.signIn("dev@wheel.dev", "nope")).rejects.toThrow("that email and password don't match");
  });

  it("turns a bare 429 into a countdown the user can act on", async () => {
    const m = await load();
    vi.stubGlobal("fetch", vi.fn(async () => respond(429, {}, { "retry-after": "30" })));
    await expect(m.signIn("dev@wheel.dev", "nope")).rejects.toThrow(/30 seconds/);
  });

  it("does not accuse the user, because the limit is keyed per account", async () => {
    const m = await load();
    vi.stubGlobal("fetch", vi.fn(async () => respond(429, {})));
    // Someone else hammering your email throttles you; "too many attempts" would blame the wrong person.
    await expect(m.signIn("dev@wheel.dev", "nope")).rejects.toThrow(/paused for this account/i);
  });

  it("reports an unreachable API as offline, not as bad credentials", async () => {
    const m = await load();
    vi.stubGlobal("fetch", vi.fn(async () => { throw new TypeError("failed to fetch"); }));
    await expect(m.signIn("dev@wheel.dev", "x")).rejects.toThrow(/can't reach the api/i);
  });
});

describe("forgetting", () => {
  it("clears local state even when the logout call fails", async () => {
    const m = await load();
    vi.stubGlobal("fetch", vi.fn(async () => respond(200, SESSION)));
    await m.signIn("dev@wheel.dev", "wheel-dev-password");

    vi.stubGlobal("fetch", vi.fn(async () => { throw new TypeError("offline"); }));
    await m.signOut();

    expect(m.sessionToken()).toBeNull();
    expect(window.localStorage.getItem("wheel.session")).toBeNull();
  });

  it("drops the session when any route 401s", async () => {
    const m = await load();
    const { notifyUnauthorized } = await import("./auth");
    vi.stubGlobal("fetch", vi.fn(async () => respond(200, SESSION)));
    await m.signIn("dev@wheel.dev", "wheel-dev-password");
    expect(m.sessionToken()).toBe("local.abc");

    notifyUnauthorized();
    expect(m.sessionToken()).toBeNull();
    expect(window.localStorage.getItem("wheel.session")).toBeNull();
  });

  it("notifies subscribers so the UI re-renders instead of showing a stale identity", async () => {
    const m = await load();
    const seen: string[] = [];
    m.subscribeSession(() => seen.push("changed"));
    vi.stubGlobal("fetch", vi.fn(async () => respond(200, SESSION)));
    await m.signIn("dev@wheel.dev", "wheel-dev-password");
    m.clearSession();
    expect(seen.length).toBeGreaterThanOrEqual(2);
  });
});

describe("signing up", () => {
  it("posts to the signup route and keeps the session it gets back", async () => {
    const m = await load();
    const fetchMock = vi.fn(async () => respond(201, SESSION));
    vi.stubGlobal("fetch", fetchMock);

    const user = await m.signUp("  Dev@wheel.dev  ", "wheel-dev-password");

    expect(user.id).toBe("u1");
    expect(m.sessionToken()).toBe("local.abc");
    const [url, init] = fetchMock.mock.calls[0] as unknown as [string, RequestInit];
    expect(url).toMatch(/\/v1\/auth\/signup$/);
    // Trimmed, because a trailing space in an email is a typo the user cannot see.
    expect(JSON.parse(init.body as string).email).toBe("Dev@wheel.dev");
  });

  it("has its own words for a taken email when the API sends none", async () => {
    const m = await load();
    vi.stubGlobal("fetch", vi.fn(async () => respond(409, undefined)));
    await expect(m.signUp("dev@wheel.dev", "wheel-dev-password")).rejects.toThrow(/already an account/i);
  });

  it.each([
    [400, /check the email and password/i],
    [500, /api failed/i],
    [418, /didn't work/i],
  ])("falls back to plain copy for a bare %i", async (status, pattern) => {
    const m = await load();
    vi.stubGlobal("fetch", vi.fn(async () => respond(status, undefined)));
    await expect(m.signIn("dev@wheel.dev", "x")).rejects.toThrow(pattern);
  });
});

describe("useSession", () => {
  it("re-renders the tree when the session changes", async () => {
    const m = await load();
    function Who() {
      const session = m.useSession();
      return <span>{session.status === "authed" ? session.user.email : session.status}</span>;
    }

    render(<Who />);
    expect(screen.getByText("loading")).toBeTruthy();

    act(() => m.hydrateSession());
    expect(screen.getByText("anon")).toBeTruthy();

    vi.stubGlobal("fetch", vi.fn(async () => respond(200, SESSION)));
    await act(async () => {
      await m.signIn("dev@wheel.dev", "wheel-dev-password");
    });
    expect(screen.getByText("dev@wheel.dev")).toBeTruthy();
  });
});

describe("mode guard", () => {
  it("does not hijack the token getter when another provider owns sessions", async () => {
    const m = await load("clerk");
    const auth = await import("./auth");
    auth.setTokenGetter(async () => "clerk-token");
    vi.stubGlobal("fetch", vi.fn(async () => respond(200, SESSION)));
    await m.signIn("dev@wheel.dev", "wheel-dev-password");
    // The local store holds a token, but the API client still asks Clerk for one.
    await expect(auth.getAuthToken()).resolves.toBe("clerk-token");
  });
});

describe("what the form checks before spending a round trip", () => {
  it.each([
    ["", "Enter your password."],
    ["short", "at least 10 characters"],
  ])("rejects %j", async (password, fragment) => {
    const m = await load();
    expect(m.passwordProblem(password)).toContain(fragment);
  });

  it("counts how many characters are still missing", async () => {
    const m = await load();
    expect(m.passwordProblem("abcdefgh")).toContain("2 more");
  });

  it("accepts a password that meets the rule", async () => {
    const m = await load();
    expect(m.passwordProblem("abcdefghij")).toBeNull();
  });

  it.each(["", "not-an-email", "no@domain", "@example.com"])("rejects the email %j", async (email) => {
    const m = await load();
    expect(m.emailProblem(email)).toBeTruthy();
  });

  it("accepts a real address, trimmed", async () => {
    const m = await load();
    expect(m.emailProblem("  dev@wheel.dev ")).toBeNull();
  });
});
