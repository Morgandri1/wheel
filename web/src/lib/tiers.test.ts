import { describe, expect, it } from "vitest";
import { TIERS, tierAtLeast, tierLabel, type Tier } from "./tiers";

describe("tierAtLeast", () => {
  it("orders the three tiers least to most capable", () => {
    expect(TIERS).toEqual(["guest", "prompter", "admin"]);
  });

  it("lets a tier satisfy itself and everything below it", () => {
    expect(tierAtLeast("admin", "admin")).toBe(true);
    expect(tierAtLeast("admin", "prompter")).toBe(true);
    expect(tierAtLeast("admin", "guest")).toBe(true);
    expect(tierAtLeast("prompter", "prompter")).toBe(true);
    expect(tierAtLeast("prompter", "guest")).toBe(true);
    expect(tierAtLeast("guest", "guest")).toBe(true);
  });

  it("refuses a tier below what is needed", () => {
    expect(tierAtLeast("guest", "prompter")).toBe(false);
    expect(tierAtLeast("guest", "admin")).toBe(false);
    expect(tierAtLeast("prompter", "admin")).toBe(false);
  });

  // A project record from an older API, or one fetched without a caller, must not read as
  // permission — and `guest` is the case that matters, because it is the lowest bar there is. If an
  // unknown value cleared *that*, it would clear everything.
  it("treats a missing or unrecognised tier as no permission at all", () => {
    for (const unknown of [undefined, "", "owner", "editor", "viewer", "root", "ADMIN", "Admin"]) {
      expect(tierAtLeast(unknown, "guest")).toBe(false);
      expect(tierAtLeast(unknown, "admin")).toBe(false);
    }
  });

  // The mutation that this whole group exists to catch: comparing by equality instead of by order.
  // `tier === "admin"` denies an admin nothing, but any equality check against a *lower* tier
  // denies an admin everything — a plausible bug that reads correctly at a glance.
  it("is an ordering, not an equality check", () => {
    const wouldBeEquality = TIERS.map((t) => tierAtLeast("admin", t));
    expect(wouldBeEquality).toEqual([true, true, true]);
  });
});

describe("tierLabel", () => {
  it("names every tier", () => {
    for (const t of TIERS) {
      expect(tierLabel(t satisfies Tier)).toMatch(/^[A-Z]/);
    }
  });
});
