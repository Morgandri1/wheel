// @vitest-environment node
// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  SECURE_SESSION_COOKIE,
  SESSION_COOKIE,
  clearedSessionCookie,
  isCookieSafeToken,
  isLiveSessionToken,
  isSecureRequest,
  liveSessionToken,
  readCookie,
  readSessionToken,
  sessionCookie,
  sessionCookieName,
  sessionMaxAge,
  signInRedirect,
} from "./session-cookie";

beforeEach(() => {
  vi.stubEnv("WHEEL_PUBLIC_ORIGIN", "");
  vi.stubEnv("WHEEL_TRUST_PROXY", "");
  vi.stubEnv("VERCEL", "");
});

afterEach(() => vi.unstubAllEnvs());

const jwt = (claims: object) =>
  `eyJhbGciOiJIUzI1NiJ9.${Buffer.from(JSON.stringify(claims)).toString("base64url")}.c2ln`;

/** `name=value; A; B=c` → { name, value, attrs } with attribute names lowercased. */
function parse(header: string) {
  const [pair, ...rest] = header.split("; ");
  const eq = pair!.indexOf("=");
  const attrs = new Map(rest.map((a) => {
    const [k, v] = a.split("=");
    return [k!.toLowerCase(), v ?? true] as const;
  }));
  return { name: pair!.slice(0, eq), value: pair!.slice(eq + 1), attrs };
}

describe("the cookie's name", () => {
  it("takes the __Host- prefix over https, which a browser only accepts from a secure origin with no Domain", () => {
    expect(sessionCookieName(true)).toBe("__Host-wheel_session");
    expect(sessionCookieName(false)).toBe("wheel_session");
    expect(SECURE_SESSION_COOKIE.startsWith("__Host-")).toBe(true);
    expect(SESSION_COOKIE).toBe("wheel_session");
  });

  it("is Secure for a direct https connection and not for plain http on localhost", () => {
    expect(isSecureRequest(new Request("https://wheel.example/"))).toBe(true);
    expect(isSecureRequest(new Request("http://localhost:3000/", { headers: { host: "localhost:3000" } }))).toBe(false);
  });

  it("is Secure behind a trusted TLS-terminating proxy", () => {
    vi.stubEnv("WHEEL_TRUST_PROXY", "1");
    expect(isSecureRequest(new Request("http://web:3000/", { headers: { "x-forwarded-proto": "https" } }))).toBe(true);
  });

  it("is Secure when WHEEL_PUBLIC_ORIGIN says https, whatever the headers claim", () => {
    vi.stubEnv("WHEEL_PUBLIC_ORIGIN", "https://wheel.example.com");
    expect(isSecureRequest(new Request("http://web:3000/", { headers: { "x-forwarded-proto": "http" } }))).toBe(true);
  });

  it("is not earned by a forged X-Forwarded-Proto from a peer nobody declared trusted", () => {
    expect(isSecureRequest(new Request("http://web:3000/", { headers: { "x-forwarded-proto": "https" } }))).toBe(false);
  });
});

describe("reading it back", () => {
  it.each<[string | null, string | null]>([
    ["wheel_session=abc", "abc"],
    ["theme=dark; wheel_session=abc; other=1", "abc"],
    ["xwheel_session=wrong; wheel_session=right", "right"],
    ["wheel_session=a=b", "a=b"],
    ["wheel_session=", null],
    ["other=1", null],
    ["garbage", null],
    [null, null],
  ])("reads %j as %j", (header, expected) => {
    expect(readCookie(header, "wheel_session")).toBe(expected);
  });

  it("over https reads only the __Host- cookie, so a plain one planted by a sibling subdomain is ignored", () => {
    const req = new Request("https://wheel.example/", {
      headers: { cookie: "wheel_session=planted; __Host-wheel_session=real" },
    });
    expect(readSessionToken(req)).toBe("real");
    expect(readSessionToken(new Request("https://wheel.example/", { headers: { cookie: "wheel_session=planted" } }))).toBeNull();
  });

  it("over http reads the plain cookie", () => {
    expect(readSessionToken(new Request("http://localhost/", { headers: { cookie: "wheel_session=t" } }))).toBe("t");
  });
});

describe("what gets written", () => {
  it("sets HttpOnly, SameSite=Lax, Path=/ and Max-Age, and no Domain", () => {
    const c = parse(sessionCookie("tok.en.x", { secure: false, maxAge: 3600 }));
    expect(c.name).toBe("wheel_session");
    expect(c.value).toBe("tok.en.x");
    expect(c.attrs.get("httponly")).toBe(true);
    expect(c.attrs.get("samesite")).toBe("Lax");
    expect(c.attrs.get("path")).toBe("/");
    expect(c.attrs.get("max-age")).toBe("3600");
    expect(c.attrs.has("domain")).toBe(false);
    expect(c.attrs.has("secure")).toBe(false);
  });

  it("adds Secure and the __Host- name on https", () => {
    const c = parse(sessionCookie("t", { secure: true, maxAge: 60 }));
    expect(c.name).toBe("__Host-wheel_session");
    expect(c.attrs.get("secure")).toBe(true);
    expect(c.attrs.get("httponly")).toBe(true);
  });

  it("leaves Max-Age off when the session's end is unknown, so it lasts the browser session only", () => {
    expect(parse(sessionCookie("t", { secure: false, maxAge: null })).attrs.has("max-age")).toBe(false);
  });

  // A clear only clears if name and path match what was set.
  it.each([true, false])("clears with the same name, flags and path (secure=%s)", (secure) => {
    const set = parse(sessionCookie("t", { secure, maxAge: 60 }));
    const cleared = parse(clearedSessionCookie(secure));
    expect(cleared.name).toBe(set.name);
    expect(cleared.value).toBe("");
    expect(cleared.attrs.get("max-age")).toBe("0");
    expect(cleared.attrs.get("path")).toBe("/");
    expect(cleared.attrs.get("httponly")).toBe(true);
    expect(cleared.attrs.has("secure")).toBe(secure);
  });

  it.each([
    ["a JWT", "eyJ.eyJ.sig-_", true],
    ["a mock token", "local.00000000-0000-4000-8000-0000000000ff", true],
    ["a semicolon", "a;b", false],
    ["a space", "a b", false],
    ["a header break", "a\r\nSet-Cookie: x=1", false],
    ["nothing", "", false],
    ["an absurd length", "a".repeat(5000), false],
  ])("treats %s as %s to write into a cookie", (_label, token, ok) => {
    expect(isCookieSafeToken(token)).toBe(ok);
  });
});

describe("how long it lasts", () => {
  const now = Date.parse("2026-09-11T12:00:00Z");

  it("matches the API's expires_at", () => {
    expect(sessionMaxAge("2026-09-11T13:00:00Z", "opaque", now)).toBe(3600);
  });

  it("falls back to the JWT's own exp when the API does not say", () => {
    expect(sessionMaxAge(undefined, jwt({ exp: now / 1000 + 90 }), now)).toBe(90);
    expect(sessionMaxAge("not a date", jwt({ exp: now / 1000 + 90 }), now)).toBe(90);
  });

  it("prefers expires_at over the JWT when both are present", () => {
    expect(sessionMaxAge("2026-09-11T12:00:30Z", jwt({ exp: now / 1000 + 9999 }), now)).toBe(30);
  });

  it.each([
    ["an opaque token", "local.00000000-0000-4000-8000-0000000000ff"],
    ["a JWT with no exp", jwt({ sub: "u1" })],
    ["a JWT whose exp is not a number", jwt({ exp: "soon" })],
    ["a payload that is not JSON", "a.bm90IGpzb24.c"],
    ["no payload at all", "justonepart"],
  ])("is unknown for %s", (_label, token) => {
    expect(sessionMaxAge(undefined, token, now)).toBeNull();
  });

  it("is zero, never negative, for a session already over", () => {
    expect(sessionMaxAge("2026-09-11T11:00:00Z", "t", now)).toBe(0);
  });
});

// Security review, finding 3: any cookie value used to count as a credential, and bought a 5 MiB
// body read or an upstream socket. Only a live JWT is worth presenting now.
describe("which cookie values are worth presenting", () => {
  const now = Date.parse("2026-09-11T12:00:00Z");
  const exp = (offsetSeconds: number) => jwt({ sub: "u1", exp: now / 1000 + offsetSeconds });

  it("is a JWT not yet past its exp", () => {
    expect(isLiveSessionToken(exp(60), now)).toBe(true);
  });

  it.each([
    ["an expired JWT", exp(-1)],
    ["a JWT expiring this instant", exp(0)],
    ["a JWT with no exp", jwt({ sub: "u1" })],
    ["a JWT whose exp is not a number", jwt({ exp: "tomorrow" })],
    ["an opaque token", "local.00000000-0000-4000-8000-0000000000ff"],
    ["two parts", "a.b"],
    ["four parts", `${exp(60)}.extra`],
    ["an empty signature", exp(60).replace(/\.[^.]+$/, ".")],
    ["a plain string", "mock-session-token"],
    ["something enormous", `${exp(60)}${"A".repeat(5000)}`],
  ])("is not %s", (_label, token) => {
    expect(isLiveSessionToken(token, now)).toBe(false);
  });

  it("reads a live cookie and drops anything else", () => {
    const live = jwt({ exp: Date.now() / 1000 + 60 });
    expect(liveSessionToken(new Request("http://localhost/", { headers: { cookie: `wheel_session=${live}` } }))).toBe(live);
    expect(liveSessionToken(new Request("http://localhost/", { headers: { cookie: "wheel_session=garbage" } }))).toBeNull();
    expect(liveSessionToken(new Request("http://localhost/"))).toBeNull();
  });
});

describe("middleware's redirect for /app", () => {
  it.each<[string, boolean, string | null]>([
    ["/app", false, "/sign-in"],
    ["/app/p1", false, "/sign-in?next=%2Fapp%2Fp1"],
    ["/app/settings", false, "/sign-in?next=%2Fapp%2Fsettings"],
    ["/app", true, null],
    ["/app/p1", true, null],
    ["/apple", false, null],
    ["/", false, null],
    ["/sign-in", false, null],
    ["/api/session", false, null],
  ])("%s with session=%s → %j", (path, has, expected) => {
    expect(signInRedirect(path, has)).toBe(expected);
  });
});
