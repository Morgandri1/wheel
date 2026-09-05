import { describe, expect, it } from "vitest";
import { NODE_TYPES } from "@/lib/schema";
import {
  suggestName,
  validateChestKey,
  validateColumnName,
  validateEndpointPath,
  validateNodeName,
} from "@/lib/validate";

/**
 * A node's name is the address other agents send to, and an endpoint's path is reachable from
 * the public internet. Both are validated here before the engine ever sees them, so these rules
 * are the ones a person actually bumps into.
 */
describe("validateNodeName", () => {
  it("accepts the shapes §3 allows", () => {
    for (const name of ["a", "0", "agent", "agent-2", "my_agent", "a".repeat(63), "a-b_c-9"]) {
      expect(validateNodeName(name), name).toBeNull();
    }
  });

  it("refuses names that could not be addressed", () => {
    expect(validateNodeName("")).toMatch(/needs a name/);
    expect(validateNodeName("-leading")).toMatch(/lowercase/);
    expect(validateNodeName("_leading")).toMatch(/lowercase/);
    expect(validateNodeName("Capital")).toMatch(/lowercase/);
    expect(validateNodeName("has space")).toMatch(/lowercase/);
    expect(validateNodeName("has.dot")).toMatch(/lowercase/);
    expect(validateNodeName("emoji🙂")).toMatch(/lowercase/);
  });

  it("reports over-long names by length rather than by charset", () => {
    // Both rules would reject it; the length message is the useful one.
    expect(validateNodeName("a".repeat(64))).toMatch(/63 characters/);
  });

  it("refuses a name already on the board, and names the clash", () => {
    expect(validateNodeName("writer", ["researcher", "writer"])).toContain("writer");
    expect(validateNodeName("writer", ["researcher"])).toBeNull();
  });
});

describe("validateEndpointPath", () => {
  it("accepts rooted paths", () => {
    expect(validateEndpointPath("/hook")).toBeNull();
    expect(validateEndpointPath("/a/b/c")).toBeNull();
    expect(validateEndpointPath("/")).toBeNull();
  });

  it("refuses anything unrooted, traversing, or ambiguous over the wire", () => {
    expect(validateEndpointPath("hook")).toMatch(/slash/);
    expect(validateEndpointPath("/../etc")).toMatch(/\.\./);
    expect(validateEndpointPath("/a b")).toMatch(/spaces/);
  });
});

describe("validateColumnName", () => {
  it("accepts sqlite-safe identifiers", () => {
    for (const n of ["claim", "_private", "col_9", "a"]) expect(validateColumnName(n), n).toBeNull();
  });

  it("refuses what could not be quoted into DDL safely", () => {
    for (const n of ["9leading", "Capital", "has space", "has-dash", "", "a".repeat(64)]) {
      expect(validateColumnName(n), n).toMatch(/lowercase/);
    }
  });
});

describe("validateChestKey", () => {
  it("accepts relative paths", () => {
    expect(validateChestKey("notes.md")).toBeNull();
    expect(validateChestKey("a/b/c.png")).toBeNull();
  });

  it("refuses absolute paths and traversal", () => {
    expect(validateChestKey("")).toMatch(/name/);
    expect(validateChestKey("/etc/passwd")).toMatch(/relative/);
    expect(validateChestKey("a/../../etc")).toMatch(/\.\./);
  });
});

describe("suggestName", () => {
  it("uses the bare type when it is free", () => {
    for (const t of NODE_TYPES) expect(suggestName(t, [])).toBe(t);
  });

  it("counts up past every taken name", () => {
    expect(suggestName("agent", ["agent"])).toBe("agent-2");
    expect(suggestName("agent", ["agent", "agent-2", "agent-3"])).toBe("agent-4");
    expect(suggestName("ctx", ["agent"])).toBe("ctx");
  });

  it("always suggests something valid, even on a board full of them", () => {
    const taken = ["agent", ...Array.from({ length: 600 }, (_, i) => `agent-${i + 2}`)];
    const suggestion = suggestName("agent", taken);
    expect(taken).not.toContain(suggestion);
    expect(validateNodeName(suggestion)).toBeNull();
  });
});
