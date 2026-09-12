// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import { describe, expect, it, vi } from "vitest";
import { errorCode, probeEndpoint, probeVerdict, unreadableReason } from "@/lib/endpoint-probe";

const reading = (status: number, body = "", statusText = "", truncated = false) =>
  vi.fn().mockResolvedValue(Response.json({ status, status_text: statusText, body, truncated })) as unknown as typeof fetch;
const answering = (res: unknown) => vi.fn().mockResolvedValue(res) as unknown as typeof fetch;
const target = { projectId: "p1", path: "/hook", method: "POST" };

describe("probing an endpoint through this app's server", () => {
  it("reports the reading the server took, verbatim, so the panel measures instead of claiming", async () => {
    const probe = await probeEndpoint(target, { fetchImpl: reading(404, "no route", "Not Found") });
    expect(probe).toEqual({
      kind: "answered",
      status: 404,
      statusText: "Not Found",
      body: "no route",
      truncated: false,
      code: null,
    });
  });

  it("asks this app's server — never the ingress or the API directly", async () => {
    const f = reading(202);
    await probeEndpoint(target, { fetchImpl: f });
    const [url, init] = (f as unknown as ReturnType<typeof vi.fn>).mock.calls[0] as [string, RequestInit];
    expect(url).toBe("/api/wheel/probe");
    expect(init).toMatchObject({ method: "POST", credentials: "same-origin", cache: "no-store" });
    expect(JSON.parse(init.body as string)).toEqual({ project_id: "p1", method: "POST", path: "/hook" });
  });

  it("probes with GET when no method is given", async () => {
    const f = reading(200);
    await probeEndpoint({ projectId: "p1", path: "/hook" }, { fetchImpl: f });
    const [, init] = (f as unknown as ReturnType<typeof vi.fn>).mock.calls[0] as [string, RequestInit];
    expect(JSON.parse(init.body as string).method).toBe("GET");
  });

  it("truncates a body that would take the panel over, and says that it did", async () => {
    const probe = await probeEndpoint(target, { fetchImpl: reading(200, "x".repeat(5000)) });
    expect(probe).toMatchObject({ kind: "answered", truncated: true });
    if (probe.kind === "answered") expect(probe.body.length).toBe(2000);
  });

  it("keeps the server's own word that it truncated", async () => {
    const probe = await probeEndpoint(target, { fetchImpl: reading(200, "short", "", true) });
    expect(probe).toMatchObject({ kind: "answered", truncated: true, body: "short" });
  });

  it("does not call a test that never ran a dead endpoint", async () => {
    const f = vi.fn().mockRejectedValue(new TypeError("Failed to fetch")) as unknown as typeof fetch;
    const probe = await probeEndpoint(target, { fetchImpl: f });
    expect(probe.kind).toBe("unreadable");
    if (probe.kind === "unreadable") {
      expect(probe.reason).toMatch(/not evidence that the endpoint is down/i);
      expect(probe.reason).toMatch(/Failed to fetch/);
    }
  });

  it("says the test did not run, in the server's words, when the server refuses it", async () => {
    const probe = await probeEndpoint(target, {
      fetchImpl: answering(Response.json({ error: { code: "not_found", message: "That's gone" } }, { status: 404 })),
    });
    expect(probe.kind).toBe("unreadable");
    if (probe.kind === "unreadable") {
      expect(probe.reason).toMatch(/did not run: That's gone/);
      expect(probe.reason).toMatch(/not evidence that the endpoint is down/i);
    }
  });

  it("names the status when a refusal carries no words", async () => {
    const probe = await probeEndpoint(target, { fetchImpl: answering(new Response("", { status: 500 })) });
    expect(probe.kind === "unreadable" && probe.reason).toMatch(/HTTP 500/);
  });

  it("signs the UI out when the server says the session is dead", async () => {
    const { setUnauthorizedHandler } = await import("@/lib/auth");
    const unauthorized = vi.fn();
    setUnauthorizedHandler(unauthorized);
    await probeEndpoint(target, { fetchImpl: answering(new Response("{}", { status: 401 })) });
    expect(unauthorized).toHaveBeenCalledOnce();
    setUnauthorizedHandler(() => {});
  });

  it.each([
    ["no status", JSON.stringify({ body: "x" })],
    ["no body", JSON.stringify({ status: 200 })],
    ["not JSON", "<html>"],
  ])("has no reading when the server's answer has %s", async (_label, text) => {
    const probe = await probeEndpoint(target, { fetchImpl: answering(new Response(text)) });
    expect(probe.kind).toBe("unreadable");
  });

  it("survives a response whose body cannot be read", async () => {
    const bad = {
      ok: true,
      status: 200,
      statusText: "OK",
      text: () => Promise.reject(new Error("stream already consumed")),
    };
    await expect(probeEndpoint(target, { fetchImpl: answering(bad) })).resolves.toMatchObject({ kind: "unreadable" });
  });
});

/**
 * QA review round 2: a script endpoint that outlives the server's own hit deadline is delivered,
 * not failed, so it must read as "sent" — never as "unreadable" (which the panel would show as
 * "the test did not run") and never as "answered" with an invented status code.
 */
describe("a hit that has not answered yet", () => {
  const sent = (timeoutMs = 30_000) => answering(Response.json({ sent: true, timeout_ms: timeoutMs }));

  it("reads the server's sent outcome as its own kind, not answered or unreadable", async () => {
    const probe = await probeEndpoint(target, { fetchImpl: sent(30_000) });
    expect(probe).toEqual({ kind: "sent", timeoutMs: 30_000 });
  });

  it("falls back to a default timeout if the server omits it, rather than failing to read the answer", async () => {
    const probe = await probeEndpoint(target, { fetchImpl: answering(Response.json({ sent: true })) });
    expect(probe).toEqual({ kind: "sent", timeoutMs: 30_000 });
  });
});

/**
 * The operator hit a bare 404 on `/tg` and could not tell "ingress is not built" from "I typed the
 * path wrong". Those two readings send someone to completely different places for an hour.
 */
describe("what a status code is allowed to claim", () => {
  // Ingress is live (340f318), so a bodiless 404 no longer means "not built" — it means an engine
  // that predates the deploy. The assertion that must survive is the one about the PATH.
  it("refuses to let a bodiless 404 read as a bad path", () => {
    const verdict = probeVerdict({ status: 404 });
    expect(verdict).toMatch(/predates ingress|restarting the project/i);
    expect(verdict).not.toMatch(/check the path/i);
  });

  /**
   * API turns a BODILESS 404 into 501 ingress_unavailable — an engine with no /ingress/* route at
   * all — and passes a 404 the engine wrote straight through. Excusing the second as "not built
   * yet" would tell someone their wrong path is fine, which is this button's own bug inverted.
   */
  it("does not excuse a 404 the engine actually wrote as a missing feature", () => {
    const verdict = probeVerdict({ status: 404, body: "<html>no such route</html>" });
    expect(verdict).toMatch(/real answer about this path/i);
    expect(verdict).not.toMatch(/not built yet/i);
    expect(verdict).not.toMatch(/does not mean your path is wrong/i);
  });

  it("says so plainly when the engine names the path as the problem", () => {
    expect(probeVerdict({ status: 404, code: "no_such_endpoint" })).toMatch(/no endpoint at this path/i);
  });

  it("uses the engine's own words once the API sends a code", () => {
    expect(probeVerdict({ status: 501, code: "ingress_unavailable" })).toMatch(
      /predates endpoint ingress — restart the project/i,
    );
    // The code outranks the status: a code is a fact, a status is an inference.
    expect(probeVerdict({ status: 404, code: "ingress_unavailable" })).toMatch(/predates endpoint ingress/i);
  });

  it("distinguishes the capability being off from nothing being served", () => {
    expect(probeVerdict({ status: 403 })).toMatch(/public HTTP is off/i);
    expect(probeVerdict({ status: 404 })).toMatch(/predates ingress/i);
  });

  /**
   * The four states are four different fixes. The operator hit two of them in one afternoon and
   * could not tell them apart, so no two of these may render as the same sentence.
   */
  it("gives each of the four states its own answer", () => {
    const verdicts = [
      probeVerdict({ status: 403 }),
      probeVerdict({ status: 501, code: "ingress_unavailable" }),
      probeVerdict({ status: 404, body: "no such route" }),
      probeVerdict({ status: 202, body: '{"accepted":true,"queued":1}' }),
    ];
    expect(new Set(verdicts).size).toBe(4);
  });

  it("reads the engine's bare error shape, not just the API's envelope", () => {
    // Verified in crates/wheel-engine/src/api/ingress.rs: its err() helper emits {"code":...}
    // with no `error` wrapper, unlike every other engine route. Reading only the wrapper made the
    // "check your path" verdict unreachable against a real board.
    expect(errorCode('{"code":"no_such_endpoint"}')).toBe("no_such_endpoint");
    expect(errorCode('{"error":{"code":"not_found","message":"x"}}')).toBe("not_found");
    expect(probeVerdict({ status: 404, code: errorCode('{"code":"no_such_endpoint"}') })).toMatch(
      /no endpoint at this path/i,
    );
  });

  it("blames the project, not the path, when the API has no such project", () => {
    // The exact body production returns for an unknown project id, captured from the deployment.
    const body = '{"error":{"code":"not_found","message":"The requested resource does not exist."}}';
    const verdict = probeVerdict({ status: 404, code: "not_found", body });
    expect(verdict).toMatch(/no project with this id/i);
    expect(verdict).not.toMatch(/check the path/i);
  });

  it("reads the count from an engine that predates the queued rename", () => {
    // 168430f renamed the 202 field delivered -> queued. Both mean rows written, and a project
    // whose engine has not been restarted still sends the old name.
    expect(probeVerdict({ status: 202, body: '{"accepted":true,"delivered":2}' })).toMatch(
      /2 wired nodes/,
    );
    expect(probeVerdict({ status: 202, body: '{"accepted":true,"delivered":0}' })).toMatch(
      /nothing is wired/i,
    );
  });

  it("reports the count ingress gave without promising the agent has it", () => {
    const one = probeVerdict({ status: 202, body: '{"accepted":true,"queued":1}' });
    expect(one).toMatch(/1 wired node\b/);
    expect(one).toMatch(/queued/i);
    // The 202 field is named `delivered` but counts rows ENQUEUED — a parked agent still counts.
    // The panel may repeat the number; it may not turn it into a claim the message was received.
    expect(one).not.toMatch(/delivered to/i);
    expect(one).not.toMatch(/real hit/i);
    expect(probeVerdict({ status: 202, body: '{"accepted":true,"queued":2}' })).toMatch(
      /2 wired nodes/,
    );
  });

  it("does not call an accepted-but-undelivered hit a delivery", () => {
    const verdict = probeVerdict({ status: 202, body: '{"accepted":true,"queued":0}' });
    expect(verdict).toMatch(/nothing is wired/i);
    expect(verdict).not.toMatch(/delivered to/i);
  });

  it("only says the endpoint answered when it actually did", () => {
    expect(probeVerdict({ status: 200 })).toBe("The endpoint answered.");
    for (const status of [403, 404, 405, 500, 501]) {
      expect(probeVerdict({ status })).not.toBe("The endpoint answered.");
    }
  });

  it("keeps the browser's own words when a read is refused", () => {
    expect(unreadableReason(new TypeError("Load failed"))).toContain("Load failed");
    expect(unreadableReason("not an error")).not.toContain("undefined");
  });
});

describe("reading the API's error envelope", () => {
  it("picks the code out of the envelope the API documents", () => {
    expect(errorCode('{"error":{"code":"ingress_unavailable","message":"no"}}')).toBe("ingress_unavailable");
  });

  it("treats a non-envelope body as carrying no code rather than throwing", () => {
    expect(errorCode("not json at all")).toBeNull();
    expect(errorCode("{}")).toBeNull();
    expect(errorCode('{"error":{}}')).toBeNull();
    expect(errorCode('{"error":"a string"}')).toBeNull();
    expect(errorCode("null")).toBeNull();
  });

  it("carries the code through a real probe, so the panel can prefer it over the status", async () => {
    const f = reading(501, '{"error":{"code":"ingress_unavailable","message":"not built"}}');
    const probe = await probeEndpoint(target, { fetchImpl: f });
    expect(probe).toMatchObject({ kind: "answered", status: 501, code: "ingress_unavailable" });
  });
});
