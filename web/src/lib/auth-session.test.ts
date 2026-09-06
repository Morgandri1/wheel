import { describe, expect, it } from "vitest";
import { ApiError } from "@/lib/auth";
import { countdown, completeFailure, expiryFrom, vaultShareNote } from "@/lib/auth-session";

const T0 = 1_700_000_000_000;

describe("expiryFrom", () => {
  it("turns seconds-from-now into an absolute deadline", () => {
    expect(expiryFrom(900, T0)).toBe(T0 + 900_000);
  });

  // An engine that commits to no deadline must not be rendered as "expires now".
  it.each([undefined, null, 0, -1, Number.NaN, Number.POSITIVE_INFINITY])(
    "treats %s as no deadline at all",
    (value) => {
      expect(expiryFrom(value as number | null | undefined, T0)).toBeNull();
    },
  );
});

describe("countdown", () => {
  it("formats whole minutes and seconds", () => {
    expect(countdown(T0 + 900_000, T0)).toBe("15:00");
    expect(countdown(T0 + 61_000, T0)).toBe("1:01");
    expect(countdown(T0 + 9_000, T0)).toBe("0:09");
  });

  it("has nothing to say without a deadline", () => {
    expect(countdown(null, T0)).toBeNull();
  });

  // The distinction the panel keys off: null means the window is gone, so retyping cannot help.
  it("returns null once the deadline has passed", () => {
    expect(countdown(T0, T0)).toBeNull();
    expect(countdown(T0 - 1, T0)).toBeNull();
  });
});

describe("completeFailure", () => {
  it("calls a 409 expired, because retyping cannot fix it", () => {
    const f = completeFailure(new ApiError(409, "session_expired", "gone"));
    expect(f.kind).toBe("expired");
    expect(f.message).toMatch(/start it again/i);
  });

  it("passes a 400 through in the engine's own words", () => {
    const f = completeFailure(new ApiError(400, "invalid", "that code is too short"));
    expect(f).toEqual({ kind: "rejected", message: "that code is too short" });
  });

  // A wrong code is a 400 now, so a 5xx must NOT be described as a possible typo — that sends
  // someone looking for a mistake they did not make.
  it.each([502, 503, 504])("blames the engine, not the code, on a %s", (status) => {
    const f = completeFailure(new ApiError(status, "gateway_timeout", "The project engine did not respond in time."));
    expect(f.message).toMatch(/engine did not answer/i);
    expect(f.message).toMatch(/code is fine/i);
    expect(f.message).not.toMatch(/code was wrong/i);
  });

  it("keeps other API errors as-is rather than inventing a cause", () => {
    expect(completeFailure(new ApiError(500, "boom", "engine fell over"))).toEqual({
      kind: "other",
      message: "engine fell over",
    });
  });

  it("says the engine was unreachable when the failure is not an API error", () => {
    expect(completeFailure(new TypeError("fetch failed")).kind).toBe("other");
    expect(completeFailure(new TypeError("fetch failed")).message).toMatch(/could not be reached/i);
  });
});

describe("vaultShareNote", () => {
  it("prefers the engine's own warning", () => {
    expect(vaultShareNote({ warning: "short-lived, expect to redo this" })).toBe(
      "short-lived, expect to redo this",
    );
  });

  it("names the expiry when given one", () => {
    const note = vaultShareNote({ expires_at: new Date(T0).toISOString() });
    expect(note).toMatch(/short-lived token, not a permanent login/i);
  });

  // Silence must not read as "permanent" — that is the misunderstanding this whole note exists for.
  it("still says short-lived when the engine says nothing", () => {
    expect(vaultShareNote({})).toMatch(/short-lived token/i);
    expect(vaultShareNote({ expires_at: "not a date" })).toMatch(/short-lived token/i);
  });

  it("says nothing when nothing was shared", () => {
    expect(vaultShareNote(null)).toBeNull();
    expect(vaultShareNote(undefined)).toBeNull();
  });
});
