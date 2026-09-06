import { describe, expect, it } from "vitest";
import { NODE_TYPES } from "@/lib/schema";
import {
  POSITION_MAX,
  POSITION_MIN,
  clampPosition,
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

/**
 * These two cases were BACKWARDS until 2026-09-06, and the fix was to the validator, not to the
 * tests: wheel-core's `Ident` (crates/wheel-core/src/name.rs) requires the first character to be
 * a lowercase letter OR A DIGIT, then `[a-z0-9_]`. So `_private` is refused by the engine and was
 * accepted here, and `9leading` is accepted by the engine and was refused here — a disagreement in
 * both directions at once, each producing a pointless round trip.
 */
describe("validateColumnName", () => {
  it("accepts sqlite-safe identifiers, including a leading digit", () => {
    for (const n of ["claim", "9leading", "col_9", "a"]) expect(validateColumnName(n), n).toBeNull();
  });

  it("refuses what could not be quoted into DDL safely", () => {
    for (const n of ["_private", "Capital", "has space", "has-dash", "", "a".repeat(64)]) {
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

describe("table node names must already be sqlite identifiers", () => {
  // A table node IS its table (t_<name>), and `-` is subtraction in SQL.
  it("refuses a hyphen, in the engine's own words", () => {
    const err = validateNodeName("my-notes", [], "table");
    expect(err).toMatch(/cannot contain/i);
    expect(err).toMatch(/_/);
  });

  it("accepts underscores and digits", () => {
    expect(validateNodeName("my_notes", [], "table")).toBeNull();
    expect(validateNodeName("notes2", [], "table")).toBeNull();
  });

  // Deliberately NOT stricter than the engine: `t_9lives` is a valid identifier, so refusing a
  // leading digit here would be the UI inventing a rule the server does not have.
  it("allows a leading digit, because the t_ prefix makes it an identifier", () => {
    expect(validateNodeName("9lives", [], "table")).toBeNull();
  });

  it("leaves every other node type free to use hyphens", () => {
    for (const type of ["agent", "ctx", "endpoint", "script", "vault", "chest", "mcp", "tool"] as const) {
      expect(validateNodeName("my-notes", [], type)).toBeNull();
    }
    expect(validateNodeName("my-notes")).toBeNull();
  });
});

describe("suggested names are names the engine will accept", () => {
  // `table-2` cannot be created, so suggesting it made the board look broken.
  it("separates a table suggestion with an underscore", () => {
    expect(suggestName("table", ["table"])).toBe("table_2");
    expect(validateNodeName(suggestName("table", ["table"]), [], "table")).toBeNull();
  });

  it("keeps hyphens for every other type", () => {
    expect(suggestName("agent", ["agent"])).toBe("agent-2");
  });
});

describe("column names match wheel-core's Ident", () => {
  it("accepts a leading digit, which the engine accepts", () => {
    expect(validateColumnName("2nd_place")).toBeNull();
  });

  it("refuses a leading underscore, which the engine refuses", () => {
    expect(validateColumnName("_hidden")).not.toBeNull();
  });

  it("refuses a hyphen", () => {
    expect(validateColumnName("first-name")).not.toBeNull();
  });
});

/**
 * Position is an i16 cell (contract 710239f). The engine rounds and clamps and returns what it
 * stored; this side must do the SAME arithmetic, or a node saves, is silently changed, and moves
 * on the next refetch — success followed by an unexplained jump.
 */
describe("board positions are an integer cell", () => {
  it("rounds to whole units, because the store has no sub-pixels", () => {
    expect(clampPosition({ x: 10.5, y: -3.2 })).toEqual({ x: 11, y: -3 });
  });

  it("clamps a far drag to the bound instead of letting it 400", () => {
    expect(clampPosition({ x: 99999, y: -99999 })).toEqual({ x: POSITION_MAX, y: POSITION_MIN });
  });

  it("keeps an ordinary position exactly as it is", () => {
    expect(clampPosition({ x: 120, y: 340 })).toEqual({ x: 120, y: 340 });
  });

  it("does not send NaN to the engine when a drag produces one", () => {
    expect(clampPosition({ x: NaN, y: Infinity })).toEqual({ x: 0, y: POSITION_MAX });
  });

  it("uses the i16 bounds the contract names, not approximations of them", () => {
    expect([POSITION_MIN, POSITION_MAX]).toEqual([-32768, 32767]);
  });
});
