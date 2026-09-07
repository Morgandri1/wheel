import { describe, expect, it } from "vitest";
import { dropOverride, settleOverrides, storedPosition, type Overrides } from "./drag-overrides";
import type { WheelNode } from "@/lib/schema";

const node = (id: string, x: number, y: number) =>
  ({ id, name: id, type: "ctx", position: { x, y }, wires: [], config: { markdown: "" } }) as unknown as WheelNode;

describe("storedPosition", () => {
  it("renders what the engine stored, not what we sent it", () => {
    expect(storedPosition({ position: { x: 11, y: 12 } } as Partial<WheelNode>, { x: 10, y: 12 })).toEqual({ x: 11, y: 12 });
  });

  it("keeps the engine's clamp even when it disagrees with ours", () => {
    expect(storedPosition({ position: { x: 32000, y: 0 } } as Partial<WheelNode>, { x: 32767, y: 0 })).toEqual({ x: 32000, y: 0 });
  });

  it("falls back to what we sent when the engine returns no position", () => {
    expect(storedPosition({} as Partial<WheelNode>, { x: 4, y: 5 })).toEqual({ x: 4, y: 5 });
    expect(storedPosition(null, { x: 4, y: 5 })).toEqual({ x: 4, y: 5 });
    expect(storedPosition({ position: { x: NaN, y: 5 } } as Partial<WheelNode>, { x: 4, y: 5 })).toEqual({ x: 4, y: 5 });
  });
});

describe("settleOverrides", () => {
  it("holds the override until the board reports the same position", () => {
    const held: Overrides = { a: { x: 11, y: 12 } };
    expect(settleOverrides(held, [node("a", 3, 4)])).toBe(held);
    expect(settleOverrides(held, [node("a", 11, 12)])).toEqual({});
  });

  it("keeps identity when nothing settles, so the effect cannot loop", () => {
    const held: Overrides = { a: { x: 11, y: 12 } };
    expect(settleOverrides(held, [node("b", 11, 12)])).toBe(held);
    expect(settleOverrides({}, [node("a", 1, 1)])).toEqual({});
  });

  it("settles one node without disturbing another still in flight", () => {
    const held: Overrides = { a: { x: 1, y: 1 }, b: { x: 2, y: 2 } };
    expect(settleOverrides(held, [node("a", 1, 1), node("b", 9, 9)])).toEqual({ b: { x: 2, y: 2 } });
  });
});

describe("dropOverride", () => {
  it("returns to the server's copy when a save fails", () => {
    expect(dropOverride({ a: { x: 1, y: 1 } }, "a")).toEqual({});
  });

  it("keeps identity for a node it does not hold", () => {
    const held: Overrides = { a: { x: 1, y: 1 } };
    expect(dropOverride(held, "zz")).toBe(held);
  });
});
