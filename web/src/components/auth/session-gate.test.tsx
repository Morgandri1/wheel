// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { SessionGate } from "./session-gate";

vi.mock("next/navigation", () => ({
  useRouter: () => ({ replace: vi.fn() }),
  usePathname: () => "/app",
}));

const retrySession = vi.hoisted(() => vi.fn());
const hydrateSession = vi.hoisted(() => vi.fn());
let sessionState: unknown = { status: "loading", user: null };
vi.mock("@/lib/local-auth", () => ({
  hydrateSession: () => hydrateSession(),
  retrySession: () => retrySession(),
  useSession: () => sessionState,
}));
vi.mock("@/lib/auth", () => ({ authMode: () => "local" }));

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

const board = (
  <div data-testid="board">board</div>
);

/**
 * QA review round 2: a session the server refuses outright (most often 403
 * `public_origin_required` behind a misconfigured proxy) must be shown, not swallowed into a
 * state this gate has no branch for — an unhandled status here falls through to the LAST `return`,
 * which renders the children. A silent fall-through is exactly the failure shape that matters:
 * a config error would otherwise show the board.
 */
describe("a session the server refused", () => {
  it("shows the server's own message and never the board", () => {
    sessionState = { status: "error", user: null, message: "set WHEEL_PUBLIC_ORIGIN" };
    render(<SessionGate>{board}</SessionGate>);
    expect(screen.getByTestId("session-error").textContent).toContain("set WHEEL_PUBLIC_ORIGIN");
    expect(screen.queryByTestId("board")).toBeNull();
  });

  it("offers a retry that calls back into the session module", () => {
    sessionState = { status: "error", user: null, message: "nope" };
    render(<SessionGate>{board}</SessionGate>);
    fireEvent.click(screen.getByTestId("btn-session-retry"));
    expect(retrySession).toHaveBeenCalledOnce();
  });
});

describe("every other state", () => {
  it.each([
    ["loading", { status: "loading", user: null }, "session-loading"],
    ["unreachable", { status: "unreachable", user: null }, "session-unreachable"],
    ["anon", { status: "anon", user: null }, "session-redirecting"],
  ])("shows its own screen for %s, never the board", (_label, state, testid) => {
    sessionState = state;
    render(<SessionGate>{board}</SessionGate>);
    expect(screen.getByTestId(testid)).toBeDefined();
    expect(screen.queryByTestId("board")).toBeNull();
  });

  it("renders the board once authed", () => {
    sessionState = { status: "authed", user: { id: "u1", email: "a@b.co" } };
    render(<SessionGate>{board}</SessionGate>);
    expect(screen.getByTestId("board")).toBeDefined();
  });
});
