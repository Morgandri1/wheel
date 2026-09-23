// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import { execFileSync, spawn, type ChildProcess } from "node:child_process";
import fs from "node:fs";
import http from "node:http";
import https from "node:https";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import { test, expect, type Page } from "@playwright/test";
import { T } from "../testids";

/**
 * E2E-proxy-* — the packaged server reached the way production reaches it: behind a reverse
 * proxy, on a different address than the one it bound.
 *
 * Why a real server, and why not `next dev`: a unit test that calls middleware() cannot see
 * this class of bug. Next's middleware adapter parses a middleware Location as ABSOLUTE (a
 * relative one answers 500), and a standalone server derives `req.url` from its bind address —
 * so building the redirect from `req.url` sent browsers to https://localhost:3000/sign-in
 * (web #150; `curl -I https://wheel.avo.so/app`). `next dev` builds `req.url` from the Host the
 * browser used, so it hides both. This spawns the real bundle (`wheel-web.mjs`) with
 * HOSTNAME=127.0.0.1 and reaches it through addresses that are NOT its own.
 *
 * The filename ends in `packaged.spec.ts` on purpose: that is the pattern both configs already
 * use to route a spec to packaged.config.ts and keep it out of the dev-server run.
 *
 * Contract pinned here is web's `publicOrigin(req)` (web/src/lib/same-origin.ts), as they
 * stated it:
 *   A. WHEEL_PUBLIC_ORIGIN set   → that origin, exactly, whatever any header says.
 *   B. WHEEL_TRUST_PROXY only    → LAST x-forwarded-proto / LAST x-forwarded-host, else Host.
 *   C. neither                   → Host header; forwarded headers ignored (a present
 *                                  x-forwarded-proto forces http:, Next fills it itself).
 * `web/scripts/probe-redirect.sh` is web's curl-level version of the same matrix; it stays a
 * local dev tool, so a change to one table should be mirrored in the other.
 *
 * Runtime env, not build env: middleware reads process.env at run time here (web checked), so
 * one bundle is respawned per regime instead of rebuilt.
 */

const BIN = path.resolve(__dirname, "../../../web/dist-pkg/bin/wheel-web.mjs");
const API = process.env.WHEEL_PKG_API_URL ?? "http://localhost:8789";
const PUBLIC = "https://wheel.example.test";
const HOSTILE_HOSTS = ["evil.example", "evil.example:443", "//evil.example", "evil.example/x"];

function freePort(): Promise<number> {
  return new Promise((resolve, reject) => {
    const s = net.createServer();
    s.once("error", reject);
    s.listen(0, "127.0.0.1", () => {
      const { port } = s.address() as net.AddressInfo;
      s.close(() => resolve(port));
    });
  });
}

interface Regime {
  publicOrigin?: string;
  trustProxy: boolean;
}

interface Server {
  port: number;
  log: () => string;
  stop: () => Promise<void>;
}

/**
 * The env is built, not inherited: a stray WHEEL_PUBLIC_ORIGIN / WHEEL_TRUST_PROXY / VERCEL in the
 * outer shell would silently turn "neither" into a different regime and the test would still pass.
 */
async function startServer(r: Regime, port?: number): Promise<Server> {
  const p = port ?? (await freePort());
  const env: NodeJS.ProcessEnv = { ...process.env };
  delete env.WHEEL_PUBLIC_ORIGIN;
  delete env.VERCEL;
  env.HOSTNAME = "127.0.0.1"; // the bind address differs from every address a client uses: the bug's precondition
  env.WHEEL_TRUST_PROXY = r.trustProxy ? "1" : "0";
  const args = [BIN, "--port", String(p), "--api", API];
  if (r.publicOrigin) args.push("--public-origin", r.publicOrigin);

  let log = "";
  const child: ChildProcess = spawn(process.execPath, args, { env, stdio: ["ignore", "pipe", "pipe"] });
  child.stdout?.on("data", (d) => (log += String(d)));
  child.stderr?.on("data", (d) => (log += String(d)));
  const exited = new Promise<void>((done) => child.once("exit", () => done()));

  const deadline = Date.now() + 90_000;
  for (;;) {
    try {
      await probe(p, "/", { host: `127.0.0.1:${p}` });
      break;
    } catch {
      if (Date.now() > deadline || child.exitCode !== null) {
        child.kill("SIGKILL");
        throw new Error(`wheel-web did not come up on :${p}\n${log}`);
      }
      await new Promise((r2) => setTimeout(r2, 250));
    }
  }
  return {
    port: p,
    log: () => log,
    stop: async () => {
      child.kill("SIGTERM");
      await Promise.race([exited, new Promise((r2) => setTimeout(r2, 5_000))]);
      child.kill("SIGKILL");
    },
  };
}

interface Reply {
  status: number;
  headers: http.IncomingHttpHeaders;
}

/** Raw request: Playwright's own client would not let a test say exactly what Host it sends. */
function probe(port: number, pathAndQuery: string, headers: Record<string, string>): Promise<Reply> {
  return new Promise((resolve, reject) => {
    const req = http.request({ host: "127.0.0.1", port, path: pathAndQuery, method: "GET", headers }, (res) => {
      res.resume();
      res.on("end", () => resolve({ status: res.statusCode ?? 0, headers: res.headers }));
    });
    req.on("error", reject);
    req.end();
  });
}

/** What a live-shaped session looks like to middleware: JWT-shaped with a future exp; the signature is not checked there. */
function liveJwt(): string {
  const b64 = (o: object) => Buffer.from(JSON.stringify(o)).toString("base64url");
  return `${b64({ alg: "HS256", typ: "JWT" })}.${b64({ sub: "u", exp: Math.floor(Date.now() / 1000) + 3600 })}.sig`;
}

function expectRedirect(res: Reply, want: string, label: string) {
  expect(res.status, `${label}: expected 307 (a 500 here is Next rejecting a non-absolute Location)`).toBe(307);
  expect(res.headers["location"], `${label}: Location`).toBe(want);
  expect(res.headers["content-security-policy"], `${label}: the redirect must still carry a CSP`).toMatch(/^default-src 'self'/);
  expect(res.headers["set-cookie"], `${label}: nothing may be minted before sign-in`).toBeUndefined();
  const cache = res.headers["cache-control"];
  expect(cache === undefined || /no-store/.test(String(cache)), `${label}: cache-control absent or no-store, got ${cache}`).toBe(true);
}

const fwd = (host: string, proto: string) => ({ host, "x-forwarded-host": host, "x-forwarded-proto": proto });

// ── A. WHEEL_PUBLIC_ORIGIN (+ trust proxy, the production shape) ─────────────────────────────
test.describe.serial("E2E-proxy-A: WHEEL_PUBLIC_ORIGIN set", () => {
  let s: Server;
  test.beforeAll(async () => {
    s = await startServer({ publicOrigin: PUBLIC, trustProxy: true });
  });
  test.afterAll(async () => s?.stop());

  test("E2E-proxy-A-exact: the redirect is the configured origin, never the bind address", async () => {
    const h = fwd("wheel.example.test", "https");
    expectRedirect(await probe(s.port, "/app", h), `${PUBLIC}/sign-in`, "/app has no next=");
    expectRedirect(
      await probe(s.port, "/app/9b1d-44", h),
      `${PUBLIC}/sign-in?next=%2Fapp%2F9b1d-44`,
      "/app/<id> carries next=",
    );
    expectRedirect(
      await probe(s.port, "/app/invite/wi_abc", h),
      `${PUBLIC}/sign-in?next=%2Fapp%2Finvite%2Fwi_abc`,
      "/app/invite/<token> carries next=",
    );
  });

  test("E2E-proxy-A-query-not-carried: the original query string is NOT carried (current behaviour, pinned)", async () => {
    expectRedirect(
      await probe(s.port, "/app/9b1d-44?secret=1&x=2", fwd("wheel.example.test", "https")),
      `${PUBLIC}/sign-in?next=%2Fapp%2F9b1d-44`,
      "query",
    );
  });

  test("E2E-proxy-A-unsteerable: forged forwarding headers cannot move the redirect", async () => {
    for (const evil of HOSTILE_HOSTS) {
      expectRedirect(
        await probe(s.port, "/app", { host: evil, "x-forwarded-host": evil, "x-forwarded-proto": "http" }),
        `${PUBLIC}/sign-in`,
        `hostile host ${JSON.stringify(evil)}`,
      );
    }
    // Only x-forwarded-proto/-host are ever read; the standard Forwarded header must stay ignored.
    expectRedirect(
      await probe(s.port, "/app", { ...fwd("wheel.example.test", "https"), forwarded: "host=evil.example;proto=http" }),
      `${PUBLIC}/sign-in`,
      "Forwarded header",
    );
  });

  test("E2E-proxy-A-rsc: an RSC/prefetch request is redirected the same way, not answered 500", async () => {
    const res = await probe(s.port, "/app?_rsc=abc", {
      ...fwd("wheel.example.test", "https"),
      rsc: "1",
      "next-router-prefetch": "1",
      "next-router-state-tree": "%5B%22%22%5D",
    });
    expectRedirect(res, `${PUBLIC}/sign-in`, "RSC prefetch of /app");
  });

  test("E2E-proxy-A-cookie-liveness: a live-shaped session is not redirected, and `__Host-` is iff https", async () => {
    const https_ = fwd("wheel.example.test", "https");
    const live = await probe(s.port, "/app", { ...https_, cookie: `__Host-wheel_session=${liveJwt()}` });
    expect(live.status, "a live __Host- cookie over an https origin must not be bounced to sign-in").not.toBe(307);
    // Same JWT under the http-flavoured name: over an https origin that is a different (absent) cookie.
    const wrongName = await probe(s.port, "/app", { ...https_, cookie: `wheel_session=${liveJwt()}` });
    expect(wrongName.status, "the plain cookie name must not count as a session over an https origin").toBe(307);
  });

  test("E2E-proxy-A-no-invalid-url: the server never logged ERR_INVALID_URL", async () => {
    expect(s.log()).not.toContain("ERR_INVALID_URL");
  });
});

// ── B. WHEEL_TRUST_PROXY only ────────────────────────────────────────────────────────────────
test.describe.serial("E2E-proxy-B: WHEEL_TRUST_PROXY only", () => {
  let s: Server;
  test.beforeAll(async () => {
    s = await startServer({ trustProxy: true });
  });
  test.afterAll(async () => s?.stop());

  test("E2E-proxy-B-forwarded: the redirect is built from the forwarded proto and host", async () => {
    expectRedirect(await probe(s.port, "/app", fwd("wheel.example.test", "https")), `${PUBLIC}/sign-in`, "forwarded");
  });

  test("E2E-proxy-B-last-value: with a comma list the LAST proto and host win", async () => {
    expectRedirect(
      await probe(s.port, "/app", {
        host: "internal.local",
        "x-forwarded-host": "first.example, wheel.example.test",
        "x-forwarded-proto": "http, https",
      }),
      `${PUBLIC}/sign-in`,
      "multi-value forwarded headers",
    );
  });

  test("E2E-proxy-B-host-fallback: with no x-forwarded-host the Host header is used", async () => {
    expectRedirect(
      await probe(s.port, "/app", { host: "wheel.example.test", "x-forwarded-proto": "https" }),
      `${PUBLIC}/sign-in`,
      "missing x-forwarded-host",
    );
  });

  test("E2E-proxy-B-no-invalid-url: the server never logged ERR_INVALID_URL", async () => {
    expect(s.log()).not.toContain("ERR_INVALID_URL");
  });
});

// ── C. neither ───────────────────────────────────────────────────────────────────────────────
test.describe.serial("E2E-proxy-C: neither (localhost-only mode)", () => {
  let s: Server;
  test.beforeAll(async () => {
    s = await startServer({ trustProxy: false });
  });
  test.afterAll(async () => s?.stop());

  test("E2E-proxy-C-host: the redirect points back at the Host the client used", async () => {
    expectRedirect(
      await probe(s.port, "/app", { host: `localhost:${s.port}` }),
      `http://localhost:${s.port}/sign-in`,
      "localhost",
    );
  });

  test("E2E-proxy-C-forwarded-ignored: forwarded headers are ignored, and a present x-forwarded-proto forces http", async () => {
    expectRedirect(
      await probe(s.port, "/app", {
        host: `localhost:${s.port}`,
        "x-forwarded-host": "evil.example",
        "x-forwarded-proto": "https",
      }),
      `http://localhost:${s.port}/sign-in`,
      "untrusted forwarded headers",
    );
  });

  test("E2E-proxy-C-nonloopback-host-reflected: a non-loopback Host is reflected (CURRENT behaviour, not a guarantee)", async () => {
    // middleware does not run the hostAllowed() loopback check that /api routes do. The
    // redirect only points a browser back at the host it just used, so web does not consider it
    // exploitable, but adversary owns that call — this pins what happens today so a change is deliberate.
    expectRedirect(
      await probe(s.port, "/app", { host: "wheel.example.test" }),
      "http://wheel.example.test/sign-in",
      "non-loopback host",
    );
  });

  test("E2E-proxy-C-cookie-name: over http the plain cookie is the session, `__Host-` is not", async () => {
    const h = { host: `localhost:${s.port}` };
    expect((await probe(s.port, "/app", { ...h, cookie: `wheel_session=${liveJwt()}` })).status).not.toBe(307);
    expect((await probe(s.port, "/app", { ...h, cookie: `__Host-wheel_session=${liveJwt()}` })).status).toBe(307);
  });

  test("E2E-proxy-C-no-invalid-url: the server never logged ERR_INVALID_URL", async () => {
    expect(s.log()).not.toContain("ERR_INVALID_URL");
  });
});

// ── Browser, through a real reverse proxy ────────────────────────────────────────────────────
interface Proxy {
  origin: string;
  close: () => Promise<void>;
}

/** Caddy-shaped: preserves the client's Host and says who was addressed via X-Forwarded-*. */
async function startProxy(port: number, upstream: number, tls?: { key: Buffer; cert: Buffer }): Promise<Proxy> {
  const handler: http.RequestListener = (req, res) => {
    const up = http.request(
      {
        host: "127.0.0.1",
        port: upstream,
        method: req.method,
        path: req.url,
        headers: { ...req.headers, "x-forwarded-host": req.headers.host ?? "", "x-forwarded-proto": tls ? "https" : "http" },
      },
      (r) => {
        res.writeHead(r.statusCode ?? 502, r.headers);
        r.pipe(res);
      },
    );
    up.on("error", () => {
      res.statusCode = 502;
      res.end("proxy: upstream unreachable");
    });
    req.pipe(up);
  };
  const server = tls ? https.createServer(tls, handler) : http.createServer(handler);
  await new Promise<void>((ok) => server.listen(port, "127.0.0.1", ok));
  return {
    origin: `${tls ? "https" : "http"}://127.0.0.1:${port}`,
    close: () => new Promise<void>((done) => server.close(() => done())),
  };
}

function selfSignedCert(): { key: Buffer; cert: Buffer } {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "wheel-e2e-tls-"));
  const key = path.join(dir, "k.pem");
  const cert = path.join(dir, "c.pem");
  execFileSync("openssl", [
    "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1", "-subj", "/CN=127.0.0.1",
    "-addext", "subjectAltName=IP:127.0.0.1", "-keyout", key, "-out", cert,
  ], { stdio: "ignore" });
  return { key: fs.readFileSync(key), cert: fs.readFileSync(cert) };
}

const escape = (s: string) => s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");

/** Every top-level navigation the browser makes, so "never leaves the public origin" is checked at each step, not just the last. */
function trackNavigations(page: Page): string[] {
  const seen: string[] = [];
  page.on("framenavigated", (f) => {
    if (f === page.mainFrame()) seen.push(f.url());
  });
  return seen;
}

for (const scheme of ["http", "https"] as const) {
  test.describe.serial(`E2E-proxy-browser[${scheme}]: behind a ${scheme} reverse proxy`, () => {
    test.use({ ignoreHTTPSErrors: true });
    let server: Server;
    let proxy: Proxy;
    const cookieName = scheme === "https" ? "__Host-wheel_session" : "wheel_session";

    test.beforeAll(async () => {
      // The proxy's port is chosen first: WHEEL_PUBLIC_ORIGIN must name the address browsers use.
      const proxyPort = await freePort();
      const origin = `${scheme}://127.0.0.1:${proxyPort}`;
      server = await startServer({ publicOrigin: origin, trustProxy: true });
      proxy = await startProxy(proxyPort, server.port, scheme === "https" ? selfSignedCert() : undefined);
    });
    test.afterAll(async () => {
      await proxy?.close();
      await server?.stop();
    });

    test("E2E-proxy-signed-out-stays-on-origin: a protected URL lands on sign-in on the address the browser used", async ({ page }) => {
      const seen = trackNavigations(page);
      await page.goto(proxy.origin + "/app", { waitUntil: "domcontentloaded" });
      await expect(page).toHaveURL(new RegExp(`^${escape(proxy.origin)}/sign-in`));
      expect(
        seen.filter((u) => new URL(u).origin !== proxy.origin),
        "the browser was sent off the origin it used (the localhost:3000 symptom)",
      ).toEqual([]);
    });

    test("E2E-proxy-signup-live-session: a fresh sign-up sets a live cookie, the next navigation sends it, and it lands on /app", async ({ page }) => {
      const seen = trackNavigations(page);
      await page.goto(proxy.origin + "/sign-up", { waitUntil: "domcontentloaded" });
      const email = `behind-proxy-${scheme}-${Date.now()}@example.test`;
      await page.getByTestId(T.emailInput).fill(email);
      await page.getByTestId(T.passwordInput).fill("correct-horse-battery");

      const signup = page.waitForResponse((r) => r.request().method() === "POST" && r.url().includes("/api/session/"));
      // noWaitAfter: a failing server answers the form instead of navigating; awaiting a
      // navigation that never comes hangs to the timeout and says nothing about why.
      await page.getByTestId(T.authSubmit).click({ noWaitAfter: true });
      const resp = await signup;
      expect(resp.status(), "sign-up should succeed").toBeLessThan(300);

      const setCookie = (await resp.allHeaders())["set-cookie"] ?? "";
      expect(setCookie, `the sign-up response set no ${cookieName}`).toContain(`${cookieName}=`);
      expect(setCookie.toLowerCase(), "the session cookie must be HttpOnly").toContain("httponly");
      if (scheme === "https") expect(setCookie.toLowerCase(), "a __Host- cookie must be Secure").toContain("secure");

      await expect(page.getByTestId(T.sessionBadge)).toContainText(email, { timeout: 20_000 });
      await expect(page).toHaveURL(new RegExp(`^${escape(proxy.origin)}/app`));

      // The very next navigation must carry it — the operator's symptom was a session that
      // existed but was not recognised, which bounces straight back to sign-in.
      const nav = page.waitForRequest((r) => r.isNavigationRequest() && new URL(r.url()).pathname === "/app");
      const back = await page.goto(proxy.origin + "/app", { waitUntil: "domcontentloaded" });
      expect((await (await nav).allHeaders())["cookie"] ?? "", "the next navigation did not send the session cookie").toContain(cookieName);
      expect(new URL(back!.url()).pathname, "a fresh session was bounced away from /app").toBe("/app");
      expect(seen.filter((u) => new URL(u).origin !== proxy.origin), "the browser left the public origin").toEqual([]);
    });

    test("E2E-proxy-signin-next: signing in through the real form returns to the deep link, on the public origin", async ({ page }) => {
      const seen = trackNavigations(page);
      await page.goto(proxy.origin + "/sign-up", { waitUntil: "domcontentloaded" });
      const email = `behind-proxy-in-${scheme}-${Date.now()}@example.test`;
      const password = "correct-horse-battery";
      await page.getByTestId(T.emailInput).fill(email);
      await page.getByTestId(T.passwordInput).fill(password);
      await page.getByTestId(T.authSubmit).click({ noWaitAfter: true });
      await expect(page.getByTestId(T.sessionBadge)).toContainText(email, { timeout: 20_000 });

      await page.context().clearCookies();
      await page.goto(proxy.origin + "/app/some-project", { waitUntil: "domcontentloaded" });
      await expect(page).toHaveURL(new RegExp(`^${escape(proxy.origin)}/sign-in\\?next=%2Fapp%2Fsome-project`));

      await page.getByTestId(T.emailInput).fill(email);
      await page.getByTestId(T.passwordInput).fill(password);
      await page.getByTestId(T.authSubmit).click({ noWaitAfter: true });
      await expect(page).toHaveURL(new RegExp(`^${escape(proxy.origin)}/app/some-project`), { timeout: 20_000 });
      expect(seen.filter((u) => new URL(u).origin !== proxy.origin), "the browser left the public origin").toEqual([]);
    });

    test("E2E-proxy-rsc-redirect-followed: a client-side RSC/prefetch of /app is redirected to the public origin", async ({ page }) => {
      const hops: { url: string; status: number; location?: string }[] = [];
      page.on("response", (res) => {
        if (new URL(res.url()).pathname === "/app") hops.push({ url: res.url(), status: res.status(), location: res.headers()["location"] });
      });
      await page.goto(proxy.origin + "/sign-in", { waitUntil: "domcontentloaded" });
      const followed = await page.evaluate(async () => {
        try {
          const res = await fetch("/app", { headers: { RSC: "1", "Next-Router-Prefetch": "1" } });
          return { url: res.url, redirected: res.redirected, status: res.status };
        } catch (e) {
          return { error: String(e) };
        }
      });
      expect(hops, "the prefetch reached the server once").toHaveLength(1);
      expect(hops[0].status).toBe(307);
      expect(hops[0].location, "the redirect the browser was handed").toBe(`${proxy.origin}/sign-in`);
      // The CSP's upgrade-insecure-requests rewrites the followed hop of an http page to https, so
      // only the https page can complete the follow; over http the assertion above is the whole point.
      if (scheme === "https") {
        expect(followed, "the followed redirect").toMatchObject({ redirected: true });
        expect(new URL((followed as { url: string }).url).origin).toBe(proxy.origin);
        expect((followed as { status: number }).status).toBeLessThan(400);
      }
    });

    test("E2E-proxy-browser-no-invalid-url: the server never logged ERR_INVALID_URL", async () => {
      expect(server.log()).not.toContain("ERR_INVALID_URL");
    });
  });
}
