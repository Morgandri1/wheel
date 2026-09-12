// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import { beforeEach, describe, expect, it } from "vitest";
import { useBoardStore } from "./board";
import type { LogLine } from "@/lib/schema";

const line = (seq: number, text = `line ${seq}`): LogLine => ({
  at: new Date().toISOString(),
  node_id: "a1",
  seq,
  stream: "stdout",
  text,
});

beforeEach(() => useBoardStore.getState().reset());

describe("appendLog — the reconnect replay path, distinct from seedLog's replace", () => {
  it("appends onto a previously seeded log rather than replacing it", () => {
    useBoardStore.getState().seedLog("a1", [line(1), line(2)]);
    useBoardStore.getState().appendLog("a1", [line(3), line(4)]);
    expect(useBoardStore.getState().logs.a1?.map((l) => l.seq)).toEqual([1, 2, 3, 4]);
  });

  it("is a no-op for an empty page, not a state churn", () => {
    useBoardStore.getState().seedLog("a1", [line(1)]);
    const before = useBoardStore.getState().logs;
    useBoardStore.getState().appendLog("a1", []);
    expect(useBoardStore.getState().logs).toBe(before);
  });

  it("starts a fresh entry when the node had never been seeded", () => {
    useBoardStore.getState().appendLog("a1", [line(1)]);
    expect(useBoardStore.getState().logs.a1?.map((l) => l.seq)).toEqual([1]);
  });

  it("caps the ring the same way seedLog does, dropping the oldest lines first", () => {
    const many = Array.from({ length: 1990 }, (_, i) => line(i + 1));
    useBoardStore.getState().seedLog("a1", many);
    useBoardStore.getState().appendLog(
      "a1",
      Array.from({ length: 20 }, (_, i) => line(1991 + i)),
    );
    const held = useBoardStore.getState().logs.a1 ?? [];
    expect(held).toHaveLength(2000);
    expect(held[0]?.seq).toBe(11);
    expect(held[held.length - 1]?.seq).toBe(2010);
  });

  it("does not disturb another node's log", () => {
    useBoardStore.getState().seedLog("a1", [line(1)]);
    useBoardStore.getState().seedLog("a2", [line(1)]);
    useBoardStore.getState().appendLog("a1", [line(2)]);
    expect(useBoardStore.getState().logs.a2?.map((l) => l.seq)).toEqual([1]);
  });
});
