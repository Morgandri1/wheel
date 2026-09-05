import { describe, expect, it } from "vitest";
import { byteLength, checkLimit, checkTextLimit, formatBytes, LIMITS, limitFraction } from "@/lib/limits";

describe("limits", () => {
  it("matches the §3c numbers exactly", () => {
    expect(LIMITS.messageBytes).toBe(262144);
    expect(LIMITS.valueBytes).toBe(1048576);
    expect(LIMITS.blobBytes).toBe(52428800);
  });

  it("counts UTF-8 bytes, not characters", () => {
    expect(byteLength("abc")).toBe(3);
    expect(byteLength("é")).toBe(2);
    expect(byteLength("🎡")).toBe(4);
    expect(byteLength("</AgentPrompt>")).toBe(14);
  });

  it("accepts a value exactly on the limit and rejects one byte over", () => {
    expect(checkLimit("messageBytes", LIMITS.messageBytes)).toBeNull();
    expect(checkLimit("messageBytes", LIMITS.messageBytes + 1)).not.toBeNull();
  });

  it("says how much to trim", () => {
    const msg = checkLimit("messageBytes", LIMITS.messageBytes + 1024);
    expect(msg).toContain("256 KiB");
    expect(msg).toContain("trim 1.0 KiB");
  });

  it("checks emoji-heavy text by bytes", () => {
    const emoji = "🎡".repeat(LIMITS.messageBytes / 4);
    expect(checkTextLimit("messageBytes", emoji)).toBeNull();
    expect(checkTextLimit("messageBytes", emoji + "🎡")).not.toBeNull();
  });

  it("formats sizes readably", () => {
    expect(formatBytes(512)).toBe("512 B");
    expect(formatBytes(2048)).toBe("2.0 KiB");
    expect(formatBytes(262144)).toBe("256 KiB");
    expect(formatBytes(52428800)).toBe("50.0 MiB");
  });

  it("clamps the fill fraction", () => {
    expect(limitFraction("messageBytes", 0)).toBe(0);
    expect(limitFraction("messageBytes", LIMITS.messageBytes / 2)).toBe(0.5);
    expect(limitFraction("messageBytes", LIMITS.messageBytes * 9)).toBe(1);
  });
});
