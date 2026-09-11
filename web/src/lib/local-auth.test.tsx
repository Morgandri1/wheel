// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, render, screen } from "@testing-library/react";

/**
 * The browser side of a cookie session. What matters: the browser never holds the token, a 401
 * from anywhere signs the UI out, "not asked yet" and "could not ask" are never mistaken for
 * "signed out", and the sign-in form still reads the API's own words through this app's server.
 *
 * Each case imports the module fresh: it holds process-wide state on purpose (one session per
 * browser). setTimeout is faked throughout, so a retry one case schedules can never fire inside
 * the next one and call its fetch.
 */

type Mod = typeof import("./local-auth");

async function load(): Promise<Mod> {
  vi.resetModules();
  return (await import("./local-auth")) as Mod;
}

function respond(status: number, body?: unknown, headers: Record<string, string> = {}) {
  return new Response(body === undefined ? null : JSON.stringify(body), { status, headers });
}

const USER = { id: "u1", email: "dev@wheel.dev" };
let fetchMock: ReturnType<typeof vi.fn>;

function call(i = 0) {
  const [url, init] = fetchMock.mock.calls[i] as [string, RequestInit | undefined];
  return { url, init, body: init?.body ? JSON.parse(init.body as string) : undefined };
}

beforeEach(() => {
  vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
  window.localStorage.clear();
  fetchMock = vi.fn();
  vi.stubGlobal("fetch", fetchMock);
});

afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("hydration", () => {
  it("starts loading, so a gate never mistakes 'not asked yet' for 'signed out'", async () => {
    const m = await load();
    expect(m.sessionSnapshot().status).toBe("loading");
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it("asks this app's server who the cookie belongs to", async () => {
    fetchMock.mockResolvedValue(respond(200, { user: USER }));
    const m = await load();
    await m.hydrateSession();
    expect(call().url).toBe("/api/session");
    expect(call().init).toMatchObject({ cache: "no-store", credentials: "same-origin" });
    expect(m.sessionSnapshot()).toEqual({ status: "authed", user: USER });
  });

  it("reads only an explicit {user: null} as signed out", async () => {
    fetchMock.mockResolvedValue(respond(200, { user: null }));
    const m = await load();
    await m.hydrateSession();
    expect(m.sessionSnapshot().status).toBe("anon");
  });

  // QA review, finding 3: an API blip at page load used to sign the UI out.
  it.each([
    ["a failure from the server", () => respond(502, { error: { code: "api_unreachable", message: "x" } })],
    ["a timeout from the server", () => respond(504, { error: { code: "api_timeout", message: "x" } })],
    ["an empty body", () => respond(200)],
    ["an answer with no user field", () => respond(200, {})],
    ["a user it cannot read", () => respond(200, { user: { id: "u1" } })],
  ])("reads %s as unreachable, not as signed out", async (_label, answer) => {
    fetchMock.mockResolvedValue(answer());
    const m = await load();
    await m.hydrateSession();
    expect(m.sessionSnapshot().status).toBe("unreachable");
  });

  it("reads a network failure as unreachable", async () => {
    fetchMock.mockRejectedValue(new TypeError("failed to fetch"));
    const m = await load();
    await m.hydrateSession();
    expect(m.sessionSnapshot().status).toBe("unreachable");
  });

  it("asks again after a blip, backing off, and settles on the answer it finally gets", async () => {
    fetchMock
      .mockRejectedValueOnce(new TypeError("offline"))
      .mockRejectedValueOnce(new TypeError("offline"))
      .mockResolvedValueOnce(respond(200, { user: USER }));
    const m = await load();
    await m.hydrateSession();
    expect(fetchMock).toHaveBeenCalledTimes(1);

    await vi.advanceTimersByTimeAsync(999);
    expect(fetchMock).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(1);
    expect(fetchMock).toHaveBeenCalledTimes(2);
    expect(m.sessionSnapshot().status).toBe("unreachable");

    await vi.advanceTimersByTimeAsync(1999);
    expect(fetchMock).toHaveBeenCalledTimes(2);
    await vi.advanceTimersByTimeAsync(1);
    expect(fetchMock).toHaveBeenCalledTimes(3);
    expect(m.sessionSnapshot()).toEqual({ status: "authed", user: USER });

    await vi.advanceTimersByTimeAsync(60_000);
    expect(fetchMock).toHaveBeenCalledTimes(3);
  });

  it("asks at once when the gate says try now, instead of waiting out the backoff", async () => {
    fetchMock.mockRejectedValueOnce(new TypeError("offline")).mockResolvedValueOnce(respond(200, { user: USER }));
    const m = await load();
    await m.hydrateSession();
    await m.retrySession();
    expect(m.sessionSnapshot().status).toBe("authed");
    await vi.advanceTimersByTimeAsync(60_000);
    expect(fetchMock).toHaveBeenCalledTimes(2);
  });

  it("asks once, however many gates call it", async () => {
    fetchMock.mockResolvedValue(respond(200, { user: USER }));
    const m = await load();
    await Promise.all([m.hydrateSession(), m.hydrateSession()]);
    await m.hydrateSession();
    expect(fetchMock).toHaveBeenCalledOnce();
  });

  it("does not let a late answer undo a sign-in that landed first", async () => {
    let answer!: (r: Response) => void;
    fetchMock.mockImplementationOnce(() => new Promise<Response>((resolve) => (answer = resolve)));
    const m = await load();
    const hydrating = m.hydrateSession();
    fetchMock.mockResolvedValueOnce(respond(200, { user: USER }));
    await m.signIn("dev@wheel.dev", "wheel-dev-password");
    answer(respond(200, { user: null }));
    await hydrating;
    expect(m.sessionSnapshot()).toEqual({ status: "authed", user: USER });
  });

  it("lets a sign-in that lands while unreachable stand, rather than the next retry", async () => {
    fetchMock.mockRejectedValueOnce(new TypeError("offline")).mockResolvedValueOnce(respond(200, { user: USER }));
    const m = await load();
    await m.hydrateSession();
    await m.signIn("dev@wheel.dev", "wheel-dev-password");
    await vi.advanceTimersByTimeAsync(60_000);
    expect(fetchMock).toHaveBeenCalledTimes(2);
    expect(m.sessionSnapshot().status).toBe("authed");
  });
});

describe("the token never reaches the browser", () => {
  it("keeps nothing in storage and holds no token, even if a server misbehaves and sends one", async () => {
    fetchMock.mockResolvedValue(respond(200, { user: USER, token: "should.not.stick" }));
    const m = await load();
    expect(await m.signIn("dev@wheel.dev", "wheel-dev-password")).toEqual(USER);
    expect(window.localStorage.length).toBe(0);
    expect(JSON.stringify(m.sessionSnapshot())).not.toContain("should.not.stick");
    expect(Object.keys(m)).not.toContain("sessionToken");
  });

  it("sends no credential of its own; the cookie travels by itself", async () => {
    fetchMock.mockResolvedValue(respond(200, { user: USER }));
    const m = await load();
    await m.signIn("dev@wheel.dev", "wheel-dev-password");
    const headers = new Headers(call().init?.headers);
    expect(headers.has("x-auth-token")).toBe(false);
    expect(headers.get("content-type")).toBe("application/json");
    expect(call().init?.credentials).toBe("same-origin");
  });
});

describe("signing in", () => {
  it("posts the trimmed email and the password to this app's server", async () => {
    fetchMock.mockResolvedValue(respond(200, { user: USER }));
    const m = await load();
    await m.signIn("  Dev@wheel.dev  ", "wheel-dev-password");
    expect(call().url).toBe("/api/session/login");
    expect(call().init?.method).toBe("POST");
    // Trimmed, because a trailing space in an email is a typo the user cannot see.
    expect(call().body).toEqual({ email: "Dev@wheel.dev", password: "wheel-dev-password" });
    expect(m.sessionSnapshot()).toEqual({ status: "authed", user: USER });
  });

  it("signs up at its own route", async () => {
    fetchMock.mockResolvedValue(respond(201, { user: USER }));
    const m = await load();
    expect((await m.signUp("dev@wheel.dev", "wheel-dev-password")).id).toBe("u1");
    expect(call().url).toBe("/api/session/signup");
  });

  it("refuses an answer it cannot read instead of pretending to be signed in", async () => {
    fetchMock.mockResolvedValue(respond(200, {}));
    const m = await load();
    await expect(m.signIn("dev@wheel.dev", "x")).rejects.toThrow(/can't read/i);
    expect(m.sessionSnapshot().status).not.toBe("authed");
  });

  it("surfaces the API's own message, which the server passes through", async () => {
    fetchMock.mockResolvedValue(
      respond(401, { error: { code: "invalid_credentials", message: "that email and password don't match" } }),
    );
    const m = await load();
    await expect(m.signIn("dev@wheel.dev", "nope")).rejects.toThrow("that email and password don't match");
  });

  it("turns a bare 429 into a countdown the user can act on", async () => {
    fetchMock.mockResolvedValue(respond(429, {}, { "retry-after": "30" }));
    const m = await load();
    await expect(m.signIn("dev@wheel.dev", "nope")).rejects.toThrow(/30 seconds/);
  });

  it("does not accuse the user, because the limit is keyed per account", async () => {
    fetchMock.mockResolvedValue(respond(429, {}));
    const m = await load();
    await expect(m.signIn("dev@wheel.dev", "nope")).rejects.toThrow(/paused for this account/i);
  });

  it("names this app's server, not the API, when nothing can be reached at all", async () => {
    fetchMock.mockRejectedValue(new TypeError("failed to fetch"));
    const m = await load();
    await expect(m.signIn("dev@wheel.dev", "x")).rejects.toThrow(/can't reach this app's server/i);
  });

  it("uses the server's words when it is the API that is down", async () => {
    fetchMock.mockResolvedValue(
      respond(502, { error: { code: "api_unreachable", message: "Can't reach the API. Check that it's running." } }),
    );
    const m = await load();
    await expect(m.signIn("dev@wheel.dev", "x")).rejects.toThrow(/can't reach the api/i);
  });

  it.each([
    [400, /check the email and password/i],
    [401, /don't match an account/i],
    [409, /already an account/i],
    [500, /api failed/i],
    [418, /didn't work/i],
  ])("falls back to plain copy for a bare %i", async (status, pattern) => {
    fetchMock.mockResolvedValue(respond(status));
    const m = await load();
    await expect(m.signIn("dev@wheel.dev", "x")).rejects.toThrow(pattern);
  });
});

describe("forgetting", () => {
  it("clears the cookie before the UI signs out, so a navigation cannot race it", async () => {
    fetchMock.mockResolvedValueOnce(respond(200, { user: USER }));
    const m = await load();
    await m.signIn("dev@wheel.dev", "wheel-dev-password");

    let statusWhileLoggingOut: string | undefined;
    fetchMock.mockImplementationOnce(async () => {
      statusWhileLoggingOut = m.sessionSnapshot().status;
      return respond(204);
    });
    await m.signOut();

    expect(call(1).url).toBe("/api/session/logout");
    expect(statusWhileLoggingOut).toBe("authed");
    expect(m.sessionSnapshot().status).toBe("anon");
  });

  it("signs the UI out even when the server cannot be reached", async () => {
    fetchMock.mockResolvedValueOnce(respond(200, { user: USER }));
    const m = await load();
    await m.signIn("dev@wheel.dev", "wheel-dev-password");
    fetchMock.mockRejectedValueOnce(new TypeError("offline"));
    await m.signOut();
    expect(m.sessionSnapshot().status).toBe("anon");
  });

  it("drops the session when any route 401s, from any state", async () => {
    fetchMock.mockRejectedValueOnce(new TypeError("offline")).mockResolvedValueOnce(respond(200, { user: USER }));
    const m = await load();
    const { notifyUnauthorized } = await import("./auth");
    await m.hydrateSession();
    notifyUnauthorized();
    expect(m.sessionSnapshot().status).toBe("anon");
    await m.signIn("dev@wheel.dev", "wheel-dev-password");
    notifyUnauthorized();
    expect(m.sessionSnapshot().status).toBe("anon");
  });

  it("notifies subscribers, and only when something changed", async () => {
    fetchMock.mockResolvedValue(respond(200, { user: USER }));
    const m = await load();
    const seen = vi.fn();
    m.subscribeSession(seen);
    await m.signIn("dev@wheel.dev", "wheel-dev-password");
    m.clearSession();
    m.clearSession();
    expect(seen).toHaveBeenCalledTimes(2);
  });
});

describe("changing the password", () => {
  it("posts both passwords, then signs out — the API ended every session, this one included", async () => {
    fetchMock.mockResolvedValueOnce(respond(200, { user: USER }));
    const m = await load();
    await m.signIn("dev@wheel.dev", "wheel-dev-password");
    fetchMock.mockResolvedValueOnce(respond(204));
    await m.changePassword("wheel-dev-password", "a-brand-new-password");
    expect(call(1).url).toBe("/api/session/password");
    expect(call(1).body).toEqual({ current_password: "wheel-dev-password", new_password: "a-brand-new-password" });
    expect(m.sessionSnapshot().status).toBe("anon");
  });

  it("refuses when not signed in, without a round trip", async () => {
    const m = await load();
    await expect(m.changePassword("a", "b")).rejects.toThrow(/not signed in/i);
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it("stays signed in when the API refuses the new password", async () => {
    fetchMock.mockResolvedValueOnce(respond(200, { user: USER }));
    const m = await load();
    await m.signIn("dev@wheel.dev", "wheel-dev-password");
    fetchMock.mockResolvedValueOnce(respond(400, { error: { code: "weak_password", message: "too short" } }));
    await expect(m.changePassword("wheel-dev-password", "short")).rejects.toThrow("too short");
    expect(m.sessionSnapshot().status).toBe("authed");
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

    fetchMock.mockResolvedValueOnce(respond(200, { user: null }));
    await act(async () => {
      await m.hydrateSession();
    });
    expect(screen.getByText("anon")).toBeTruthy();

    fetchMock.mockResolvedValueOnce(respond(200, { user: USER }));
    await act(async () => {
      await m.signIn("dev@wheel.dev", "wheel-dev-password");
    });
    expect(screen.getByText("dev@wheel.dev")).toBeTruthy();
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
