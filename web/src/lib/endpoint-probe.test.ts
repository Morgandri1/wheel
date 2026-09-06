import { describe, expect, it, vi } from "vitest";
import { errorCode, probeEndpoint, probeVerdict, unreadableReason } from "@/lib/endpoint-probe";

const respond = (status: number, body = "", statusText = "") =>
  vi.fn().mockResolvedValue(new Response(body, { status, statusText })) as unknown as typeof fetch;

describe("probing an endpoint's public URL", () => {
  it("reports the status and body verbatim, so the panel measures instead of claiming", async () => {
    const probe = await probeEndpoint("https://api.example/p/1/hook", { fetchImpl: respond(404, "no route") });
    expect(probe).toMatchObject({ kind: "answered", status: 404, body: "no route" });
  });

  it("never sends credentials to a URL that is public by definition", async () => {
    const f = respond(200);
    await probeEndpoint("https://api.example/p/1/hook", { fetchImpl: f });
    expect(f).toHaveBeenCalledWith(
      "https://api.example/p/1/hook",
      expect.objectContaining({ credentials: "omit", cache: "no-store" }),
    );
  });

  it("truncates a body that would take the panel over, and says that it did", async () => {
    const probe = await probeEndpoint("https://api.example/p/1/hook", { fetchImpl: respond(200, "x".repeat(5000)) });
    expect(probe).toMatchObject({ kind: "answered", truncated: true });
    if (probe.kind === "answered") expect(probe.body.length).toBe(2000);
  });

  it("does not call a blocked read a failure — the browser refuses to say which it was", async () => {
    const f = vi.fn().mockRejectedValue(new TypeError("Failed to fetch")) as unknown as typeof fetch;
    const probe = await probeEndpoint("https://api.example/p/1/hook", { fetchImpl: f });
    expect(probe.kind).toBe("unreadable");
    if (probe.kind === "unreadable") {
      expect(probe.reason).toMatch(/not evidence that the endpoint is down/i);
      expect(probe.reason).toMatch(/Failed to fetch/);
    }
  });

  it("survives a response whose body cannot be read", async () => {
    const bad = {
      status: 200,
      statusText: "OK",
      text: () => Promise.reject(new Error("stream already consumed")),
    };
    const f = vi.fn().mockResolvedValue(bad) as unknown as typeof fetch;
    await expect(probeEndpoint("https://api.example/p/1/hook", { fetchImpl: f })).resolves.toMatchObject({
      kind: "answered",
      body: "",
    });
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
      probeVerdict({ status: 202, body: '{"accepted":true,"delivered":1}' }),
    ];
    expect(new Set(verdicts).size).toBe(4);
  });

  it("reports a real delivery as a delivery, with the count ingress gave", () => {
    expect(probeVerdict({ status: 202, body: '{"accepted":true,"delivered":1}' })).toMatch(
      /delivered to 1 wired node\b/i,
    );
    expect(probeVerdict({ status: 202, body: '{"accepted":true,"delivered":2}' })).toMatch(
      /delivered to 2 wired nodes/i,
    );
  });

  it("does not call an accepted-but-undelivered hit a delivery", () => {
    const verdict = probeVerdict({ status: 202, body: '{"accepted":true,"delivered":0}' });
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
    const f = vi
      .fn()
      .mockResolvedValue(
        new Response('{"error":{"code":"ingress_unavailable","message":"not built"}}', { status: 501 }),
      ) as unknown as typeof fetch;
    const probe = await probeEndpoint("https://api.example/p/1/hook", { fetchImpl: f });
    expect(probe).toMatchObject({ kind: "answered", status: 501, code: "ingress_unavailable" });
  });
});
