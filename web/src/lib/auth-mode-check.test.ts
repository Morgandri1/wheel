// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import { describe, expect, it } from "vitest";
import { authModeMismatch, serverAuthMode } from "./auth-mode-check";

describe("authModeMismatch", () => {
  it("accepts clerk against jwks — different words, same deployment", () => {
    // The trap: a string equality check calls this a mismatch and gets itself ignored.
    expect(authModeMismatch("clerk", "jwks")).toBeNull();
  });

  it("accepts local against local, and dev against local", () => {
    expect(authModeMismatch("local", "local")).toBeNull();
    expect(authModeMismatch("dev", "local")).toBeNull();
  });

  it("names both sides and both env vars when they disagree", () => {
    const m = authModeMismatch("local", "jwks") ?? "";
    expect(m).toContain("WHEEL_AUTH_MODE");
    expect(m).toContain("AUTH_MODE on the API");
    expect(m).toContain("jwks");
  });

  it("catches clerk pointed at a local API", () => {
    expect(authModeMismatch("clerk", "local")).toContain("Nobody will be able to log in");
  });

  it("flags mock talking to a real API, and says the env var is probably unset", () => {
    // The web server's mode defaults to "mock" when WHEEL_AUTH_MODE is missing, so this is what a
    // forgotten env var looks like in production: a fixed fake token sent to a real server.
    const m = authModeMismatch("mock", "local") ?? "";
    expect(m).toContain("mock");
    expect(m).toContain("WHEEL_AUTH_MODE is probably unset");
  });
});

describe("serverAuthMode", () => {
  it("reads the field the API sends", () => {
    expect(serverAuthMode({ status: "ok", auth_mode: "jwks" })).toBe("jwks");
    expect(serverAuthMode({ status: "ok", auth_mode: "local" })).toBe("local");
  });

  it("refuses anything that is not an answer", () => {
    expect(serverAuthMode({ status: "ok" })).toBeNull();
    expect(serverAuthMode({ auth_mode: "sometimes" })).toBeNull();
    expect(serverAuthMode(null)).toBeNull();
    expect(serverAuthMode("ok")).toBeNull();
  });
});
