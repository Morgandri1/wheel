import { describe, expect, it } from "vitest";
import { AGENT_STATUSES, NODE_TYPES, WIRE_TYPES } from "@/lib/schema";
import { AGENT_STATUS_META, NODE_META, PALETTE_ORDER, WIRE_META } from "@/lib/node-meta";

/**
 * Presentation metadata is a lookup table, and the failure mode of a lookup table is a missing
 * key at render time — a node type nobody drew a glyph for, or a status the engine started
 * sending that renders as `undefined`. Both have already happened once here.
 */
describe("node metadata", () => {
  it("has an entry for every node type the engine knows about", () => {
    for (const type of NODE_TYPES) {
      expect(NODE_META[type], type).toBeDefined();
      expect(NODE_META[type].type).toBe(type);
    }
    expect(Object.keys(NODE_META).sort()).toEqual([...NODE_TYPES].sort());
  });

  it("gives every type a drawable glyph and a colour token", () => {
    for (const type of NODE_TYPES) {
      const meta = NODE_META[type];
      expect(meta.glyph, type).toMatch(/^M[\d.\s]/);
      expect(meta.tint, type).toMatch(/^var\(--t-[a-z]+\)$/);
    }
  });

  it("gives every type a label and a blurb that says what it is for", () => {
    for (const type of NODE_TYPES) {
      const meta = NODE_META[type];
      expect(meta.label.length, type).toBeGreaterThan(2);
      expect(meta.blurb.length, type).toBeGreaterThan(30);
      // The palette is where someone learns the model, so a blurb is a sentence, not a noun.
      expect(meta.blurb, type).toMatch(/\.$/);
    }
  });

  it("names each type distinctly — two identical labels make the palette useless", () => {
    const labels = NODE_TYPES.map((t) => NODE_META[t].label);
    expect(new Set(labels).size).toBe(labels.length);
    const tints = NODE_TYPES.map((t) => NODE_META[t].tint);
    expect(new Set(tints).size).toBe(tints.length);
  });

  it("offers every type in the palette exactly once, agent first", () => {
    expect([...PALETTE_ORDER].sort()).toEqual([...NODE_TYPES].sort());
    expect(new Set(PALETTE_ORDER).size).toBe(PALETTE_ORDER.length);
    expect(PALETTE_ORDER[0]).toBe("agent");
    expect(PALETTE_ORDER[1]).toBe("ctx");
  });
});

describe("wire metadata", () => {
  it("styles all three wire types distinctly", () => {
    for (const type of WIRE_TYPES) {
      expect(WIRE_META[type], type).toBeDefined();
      expect(WIRE_META[type].label).toBe(type);
    }
    const colors = WIRE_TYPES.map((t) => WIRE_META[t].color);
    expect(new Set(colors).size).toBe(colors.length);
  });

  it("distinguishes send by stroke as well as colour, so colour is never the only cue", () => {
    expect(WIRE_META.send.dash).not.toBe("0");
    expect(WIRE_META.read.dash).toBe("0");
    expect(WIRE_META.write.dash).toBe("0");
  });
});

describe("agent status metadata", () => {
  it("has an entry for every status, including parked and budget_exhausted", () => {
    for (const status of AGENT_STATUSES) {
      expect(AGENT_STATUS_META[status], status).toBeDefined();
      expect(AGENT_STATUS_META[status].label.length, status).toBeGreaterThan(2);
    }
    expect(Object.keys(AGENT_STATUS_META).sort()).toEqual([...AGENT_STATUSES].sort());
  });

  it("pulses only while something is actually happening", () => {
    expect(AGENT_STATUS_META.running.pulse).toBe(true);
    expect(AGENT_STATUS_META.starting.pulse).toBe(true);
    for (const status of ["stopped", "idle", "parked", "error", "needs_auth"] as const) {
      expect(AGENT_STATUS_META[status].pulse, status).toBe(false);
    }
  });

  it("reserves the danger colour for states that need a person", () => {
    expect(AGENT_STATUS_META.error.color).toBe("var(--danger)");
    expect(AGENT_STATUS_META.needs_auth.color).toBe("var(--danger)");
    expect(AGENT_STATUS_META.running.color).toBe("var(--live)");
    expect(AGENT_STATUS_META.parked.color).not.toBe("var(--danger)");
  });
});
