import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { AuthFlow, authView, credentialLabel, refusalCallout } from "@/components/inspector/auth-flow";
import type { EngineApi } from "@/lib/api";

afterEach(cleanup);

type Status = { authenticated: boolean; mode?: string | null; source?: string | null; account?: string };

function renderFlow({
  status,
  needsAuth = false,
  vaults = [],
}: {
  /** A promise, so a test can hold `/auth` open and inspect the paint before it answers. */
  status: Promise<Status>;
  needsAuth?: boolean;
  vaults?: string[];
}) {
  const authComplete = vi.fn().mockResolvedValue({ authenticated: true, mode: "api_key" });
  const authBegin = vi.fn().mockResolvedValue({
    session: "s1",
    mode: "paste_code",
    instructions: "open the link",
    url: "https://claude.ai/oauth/authorize",
    expires_in: 900,
  });
  const api = {
    agent: () => ({ authStatus: () => status, authBegin, authComplete }),
  } as unknown as EngineApi;
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const utils = render(
    <QueryClientProvider client={client}>
      <AuthFlow api={api} nodeId="n1" needsAuth={needsAuth} vaults={vaults} onAuthenticated={() => {}} />
    </QueryClientProvider>,
  );
  return { ...utils, authComplete, authBegin };
}

const settled = (s: Status) => Promise.resolve(s);
const never = () => new Promise<Status>(() => {});

/**
 * The rendering decision, isolated from the rendering.
 *
 * The operator's bug — "the sign-in dialog opens for a moment before disappearing" — was entirely
 * in this table: `?? false` collapsed "not read yet" into "no credential".
 */
describe("authView", () => {
  const base = { credential: null, unreadable: false, agentRefusedCredentials: false, replacing: false };

  it("waits rather than guessing while /auth has not answered", () => {
    expect(authView(base)).toBe("checking");
  });

  it("shows the stored credential only once it has actually been read", () => {
    expect(authView({ ...base, credential: { authenticated: true } })).toBe("stored");
  });

  it("asks for a credential when the engine says there is none", () => {
    expect(authView({ ...base, credential: { authenticated: false } })).toBe("sign-in");
  });

  it("lets the agent's own refusal outrank a stored credential", () => {
    expect(authView({ ...base, credential: { authenticated: true }, agentRefusedCredentials: true })).toBe("sign-in");
  });

  it("keeps sign-in reachable when /auth cannot be read at all", () => {
    expect(authView({ ...base, unreadable: true })).toBe("sign-in");
  });

  it("never leaves someone who asked to replace a credential staring at a chip", () => {
    expect(authView({ ...base, credential: { authenticated: true }, replacing: true })).toBe("sign-in");
  });
});

describe("first paint", () => {
  it("paints a placeholder, not the sign-in form, before /auth answers", () => {
    renderFlow({ status: never() });
    expect(screen.getByTestId("auth-pending")).toBeDefined();
    expect(screen.queryByTestId("auth-flow")).toBeNull();
    expect(screen.queryByTestId("btn-auth-oauth")).toBeNull();
    expect(screen.queryByTestId("auth-status")).toBeNull();
  });

  it("goes straight from the placeholder to the stored chip, never through the form", async () => {
    renderFlow({ status: settled({ authenticated: true, mode: "env", source: "anthropic-team" }) });
    expect(screen.queryByTestId("auth-flow")).toBeNull();
    await screen.findByTestId("auth-status");
    expect(screen.queryByTestId("auth-flow")).toBeNull();
  });

  it("shows the form immediately when the agent itself refused, without waiting on /auth", () => {
    renderFlow({ status: never(), needsAuth: true });
    expect(screen.getByTestId("auth-flow")).toBeDefined();
    expect(screen.queryByTestId("auth-pending")).toBeNull();
  });
});

describe("a credential that comes from a vault", () => {
  it("offers a way to sign in rather than ending in a dead sentence", async () => {
    renderFlow({
      status: settled({ authenticated: true, mode: "env", source: "anthropic-team" }),
      vaults: ["anthropic-team", "openai-ci"],
    });
    const chip = await screen.findByTestId("auth-status");
    expect(chip.textContent).toContain("anthropic-team");
    expect(screen.getByTestId("btn-auth-different-account")).toBeDefined();
  });

  it("opens the same sign-in panel, aimed at the vault the credential came from", async () => {
    renderFlow({
      status: settled({ authenticated: true, mode: "env", source: "anthropic-team" }),
      vaults: ["anthropic-team", "openai-ci"],
    });
    fireEvent.click(await screen.findByTestId("btn-auth-different-account"));

    expect(screen.getByTestId("auth-flow")).toBeDefined();
    expect(screen.getByTestId("btn-auth-oauth")).toBeDefined();
    // Replacing a vault-provided credential means replacing the VAULT's value; shadowing it with a
    // private copy leaves every other agent on the old one.
    const select = screen.getByTestId("select-auth-vault-other") as HTMLSelectElement;
    expect(select.value).toBe("anthropic-team");
  });
});

describe("an agent that refused a credential it has", () => {
  it("shows both facts and the way out", async () => {
    renderFlow({
      status: settled({ authenticated: true, mode: "env", source: "anthropic-team" }),
      needsAuth: true,
      vaults: ["anthropic-team"],
    });
    await waitFor(() => expect(screen.getByTestId("auth-status")).toBeDefined());
    expect(screen.getByTestId("auth-flow")).toBeDefined();
    const callout = screen.getByTestId("auth-needs-auth-callout").textContent ?? "";
    expect(callout).toContain("anthropic-team");
    expect(callout).toMatch(/stored is not the same as working/i);
  });
});

describe("the other-ways disclosure", () => {
  it("keeps the API key hidden until asked for", () => {
    renderFlow({ status: settled({ authenticated: false, mode: null }), needsAuth: true });
    expect(screen.queryByTestId("auth-other-ways")).toBeNull();
    expect(screen.queryByTestId("input-api-key")).toBeNull();
  });

  it("opens on click and says so, both in the label and to a screen reader", () => {
    renderFlow({ status: settled({ authenticated: false, mode: null }), needsAuth: true });
    const toggle = screen.getByTestId("btn-auth-other-ways");
    expect(toggle.getAttribute("aria-expanded")).toBe("false");
    fireEvent.click(toggle);
    expect(screen.getByTestId("auth-other-ways")).toBeDefined();
    expect(screen.getByTestId("input-api-key")).toBeDefined();
    expect(screen.getByTestId("btn-auth-other-ways").getAttribute("aria-expanded")).toBe("true");
    expect(screen.getByTestId("btn-auth-other-ways").textContent).toMatch(/^Hide/);
  });

  it("closes again, so the control is not one-way", () => {
    renderFlow({ status: settled({ authenticated: false, mode: null }), needsAuth: true });
    fireEvent.click(screen.getByTestId("btn-auth-other-ways"));
    fireEvent.click(screen.getByTestId("btn-auth-other-ways"));
    expect(screen.queryByTestId("auth-other-ways")).toBeNull();
  });
});

describe("what the panel claims about a credential", () => {
  it("says stored, never connected — only the harness can say that", () => {
    expect(credentialLabel("api_key", null, [])).toMatch(/saved/i);
    expect(credentialLabel("api_key", null, [])).not.toMatch(/connected|working|valid/i);
  });

  it("names the vault the engine named, and does not guess between several", () => {
    expect(credentialLabel("env", "anthropic-team", ["a", "b"])).toContain("anthropic-team");
    expect(credentialLabel("env", null, ["a", "b"])).toContain("one of a, b");
  });

  it("stays truthful when the engine grows a credential kind this build has never seen", () => {
    expect(credentialLabel("passkey_2027", null, [])).toBe("Credentials saved");
  });

  it("does not tell someone their agent has no credentials when it has one it refused", () => {
    expect(refusalCallout(false, null)).toMatch(/no usable credentials/i);
    expect(refusalCallout(true, "anthropic-team")).not.toMatch(/no usable credentials/i);
  });
});
