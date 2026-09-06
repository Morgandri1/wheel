import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { AuthFlow } from "@/components/inspector/auth-flow";
import type { EngineApi } from "@/lib/api";

/**
 * The disclosure that hides the API key.
 *
 * Contract §2 makes the account sign-in the native path and API keys "a hidden advanced
 * fallback", so the fallback lives behind a toggle. A toggle is only honest while it is the sole
 * owner of the state it describes — the case tested here is the one where it is not.
 */
afterEach(cleanup);

function renderFlow(authenticated: boolean) {
  const api = {
    agent: () => ({
      authStatus: vi.fn().mockResolvedValue(
        authenticated ? { authenticated: true, mode: "api_key" } : { authenticated: false, mode: null },
      ),
      authBegin: vi.fn(),
      authComplete: vi.fn(),
    }),
  } as unknown as EngineApi;
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={client}>
      <AuthFlow api={api} nodeId="n1" needsAuth={!authenticated} onAuthenticated={() => {}} />
    </QueryClientProvider>,
  );
}

describe("the other-ways disclosure", () => {
  it("keeps the API key hidden until asked for", () => {
    renderFlow(false);
    expect(screen.queryByTestId("auth-other-ways")).toBeNull();
    expect(screen.queryByTestId("input-api-key")).toBeNull();
  });

  it("opens on click and says so, both in the label and to a screen reader", () => {
    renderFlow(false);
    const toggle = screen.getByTestId("btn-auth-other-ways");
    expect(toggle.getAttribute("aria-expanded")).toBe("false");
    fireEvent.click(toggle);
    expect(screen.getByTestId("auth-other-ways")).toBeDefined();
    expect(screen.getByTestId("input-api-key")).toBeDefined();
    expect(screen.getByTestId("btn-auth-other-ways").getAttribute("aria-expanded")).toBe("true");
    expect(screen.getByTestId("btn-auth-other-ways").textContent).toMatch(/^Hide/);
  });

  it("closes again, so the control is not one-way", () => {
    renderFlow(false);
    fireEvent.click(screen.getByTestId("btn-auth-other-ways"));
    fireEvent.click(screen.getByTestId("btn-auth-other-ways"));
    expect(screen.queryByTestId("auth-other-ways")).toBeNull();
  });
});
