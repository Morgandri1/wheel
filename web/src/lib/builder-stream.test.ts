// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import { describe, expect, it } from "vitest";
import { BuilderStream, isCredentialProblem, readRefusal } from "./builder-stream";

const frame = (event: string, data: unknown) => `event: ${event}\ndata: ${JSON.stringify(data)}\n\n`;

describe("reading the builder's stream", () => {
  it("reads the frames the engine documents", () => {
    const stream = new BuilderStream();
    const frames = stream.feed(
      frame("delta", { text: "Here " }) +
        frame("delta", { text: "you go." }) +
        frame("done", { text: "Here you go.", boards: 1 }),
    );
    expect(frames).toEqual([
      { kind: "delta", text: "Here " },
      { kind: "delta", text: "you go." },
      { kind: "done", text: "Here you go.", boards: 1 },
    ]);
  });

  /**
   * A chunk is whatever the network handed over. Splitting mid-frame — or mid-word — must not lose
   * or duplicate anything, which is the one property a parser like this actually has to have.
   */
  it("survives a split anywhere, including inside a word", () => {
    const whole = frame("delta", { text: "hello" }) + frame("done", { text: "hello", boards: 0 });
    for (let cut = 1; cut < whole.length; cut++) {
      const stream = new BuilderStream();
      const frames = [...stream.feed(whole.slice(0, cut)), ...stream.feed(whole.slice(cut))];
      expect(frames).toEqual([
        { kind: "delta", text: "hello" },
        { kind: "done", text: "hello", boards: 0 },
      ]);
    }
  });

  it("holds an unterminated frame until it is terminated", () => {
    const stream = new BuilderStream();
    expect(stream.feed("event: delta\ndata: {\"text\":\"half")).toEqual([]);
    expect(stream.feed('"}\n\n')).toEqual([{ kind: "delta", text: "half" }]);
  });

  it("ignores heartbeats and anything it cannot read, rather than dying mid-answer", () => {
    const stream = new BuilderStream();
    const frames = stream.feed(
      ": keepalive\n\n" +
        "event: delta\ndata: not json\n\n" +
        "event: invented-later\ndata: {}\n\n" +
        "event: delta\ndata: {}\n\n" +
        frame("delta", { text: "real" }),
    );
    expect(frames).toEqual([{ kind: "delta", text: "real" }]);
  });

  it("keeps an error frame's code only when it is one the client knows", () => {
    const stream = new BuilderStream();
    expect(stream.feed(frame("error", { code: "needs_auth", message: "sign in" }))).toEqual([
      { kind: "error", code: "needs_auth", message: "sign in" },
    ]);
    // An unknown code still stops the run; it just cannot claim to be a credential problem.
    expect(stream.feed(frame("error", { code: "something_new", message: "?" }))).toEqual([
      { kind: "error", code: "builder_error", message: "?" },
    ]);
  });

  it("fills in a done frame the server sent thin", () => {
    const stream = new BuilderStream();
    expect(stream.feed("event: done\ndata: {}\n\n")).toEqual([{ kind: "done", text: "", boards: 0 }]);
  });

  it("reads a multi-line data payload as one frame", () => {
    const stream = new BuilderStream();
    expect(stream.feed('event: done\ndata: {"text":"a",\ndata: "boards":1}\n\n')).toEqual([
      { kind: "done", text: "a", boards: 1 },
    ]);
  });
});

describe("a refusal that arrived instead of a stream", () => {
  it("prefers the server's own words", () => {
    const refusal = readRefusal(409, {
      error: { code: "needs_auth", message: "the builder has no credential in this project" },
      sources: { agents: [{ id: "a1", name: "worker" }], vaults: [] },
    });
    expect(refusal.code).toBe("needs_auth");
    expect(refusal.message).toMatch(/no credential/);
    expect(refusal.sources).toEqual({ agents: [{ id: "a1", name: "worker" }], vaults: [] });
  });

  it("still says something useful when the body is missing or unreadable", () => {
    expect(readRefusal(429, null).message).toMatch(/already working/i);
    expect(readRefusal(413, undefined).message).toMatch(/too large/i);
    expect(readRefusal(403, { error: { message: "   " } }).message).toMatch(/credential/i);
    expect(readRefusal(500, {}).code).toBe("http_500");
  });

  it("drops a source entry that is not one, rather than rendering a blank option", () => {
    const refusal = readRefusal(409, {
      error: { code: "needs_auth", message: "x" },
      sources: { agents: [{ id: "a1", name: "ok" }, { id: 7 }, null], vaults: "nope" },
    });
    expect(refusal.sources).toEqual({ agents: [{ id: "a1", name: "ok" }], vaults: [] });
  });

  it("has no sources at all when the server offered none", () => {
    expect(readRefusal(400, { error: { code: "invalid", message: "bad" } }).sources).toBeUndefined();
  });
});

describe("which refusals the user can do something about", () => {
  it("names the ones that are about a credential", () => {
    for (const code of ["needs_auth", "policy", "ambiguous_credential"]) {
      expect(isCredentialProblem(code)).toBe(true);
    }
    for (const code of ["builder_busy", "invalid", "builder_error", "timeout"]) {
      expect(isCredentialProblem(code)).toBe(false);
    }
  });
});
