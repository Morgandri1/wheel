import { describe, expect, it } from "vitest";
import { MIN_PASSWORD_LENGTH, passwordChangeProblem } from "./password";

const good = "a-long-enough-password";

describe("passwordChangeProblem", () => {
  it("accepts a real change", () => {
    expect(passwordChangeProblem("old-password-1", good, good)).toBeNull();
  });

  it("refuses a change to the SAME password", () => {
    /**
     * The API would accept this, return 204, and revoke every session — so the user is signed out,
     * told it worked, and has changed nothing. The most confusing possible result of a security
     * action, and it costs one comparison to prevent.
     */
    expect(passwordChangeProblem(good, good, good)).toMatch(/same as the current/i);
  });

  it("catches a mistyped confirmation before it costs a session", () => {
    expect(passwordChangeProblem("old-password-1", good, `${good}x`)).toMatch(/do not match/i);
  });

  it("enforces the API's own minimum rather than letting the server reject it", () => {
    expect(passwordChangeProblem("old-password-1", "short", "short")).toMatch(
      new RegExp(`${MIN_PASSWORD_LENGTH} characters`),
    );
  });

  it("names the missing field rather than failing silently", () => {
    expect(passwordChangeProblem("", good, good)).toMatch(/current password/i);
    expect(passwordChangeProblem("old-password-1", "", "")).toMatch(/new password/i);
  });
});
