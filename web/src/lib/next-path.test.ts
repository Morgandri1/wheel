// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import { describe, expect, it } from "vitest";
import { safeNextPath } from "./next-path";

const ORIGIN = "https://wheel.example.com";

describe("where sign-in sends you next", () => {
  it.each([
    ["/app/x", "/app/x"],
    ["/app/x?tab=log#end", "/app/x?tab=log#end"],
    ["/app", "/app"],
  ])("keeps a path on this origin: %j", (raw, expected) => {
    expect(safeNextPath(raw, ORIGIN)).toBe(expected);
  });

  it.each([
    ["a protocol-relative URL", "//evil.com"],
    ["a backslash the browser reads as a slash", "/\\evil.com"],
    ["a mixed slash and backslash", "/\\/evil.com"],
    ["an absolute URL", "https://evil.com"],
    ["a scheme", "javascript:alert(1)"],
    ["a tab the browser strips", "/\t/evil.com"],
    ["a leading space", " /app"],
    ["nothing", ""],
    ["no parameter at all", null],
  ])("refuses %s", (_label, raw) => {
    expect(safeNextPath(raw, ORIGIN)).toBe("/app");
  });

  it("keeps an encoded backslash as a path on this origin, where it is harmless", () => {
    const next = safeNextPath("/%5cevil.com", ORIGIN);
    expect(new URL(next, ORIGIN).origin).toBe(ORIGIN);
    expect(next).toBe("/%5cevil.com");
  });

  it.each(["//evil.com", "/\\evil.com", "/%5cevil.com", "https://evil.com", "/app/x", "/\t/evil.com", "/..//evil.com"])(
    "never leaves the origin, whatever the input: %j",
    (raw) => {
      expect(new URL(safeNextPath(raw, ORIGIN), ORIGIN).origin).toBe(ORIGIN);
    },
  );
});
