// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MembersPanel } from "./members-panel";
import type { CreatedInvite, InviteInfo, MemberList } from "@/lib/schema";
import type { Tier } from "@/lib/tiers";

const list = vi.hoisted(() => vi.fn());
const grant = vi.hoisted(() => vi.fn());
const revoke = vi.hoisted(() => vi.fn());
const inviteList = vi.hoisted(() => vi.fn());
const inviteCreate = vi.hoisted(() => vi.fn());
const inviteRevoke = vi.hoisted(() => vi.fn());

vi.mock("@/lib/api", async (orig) => ({
  ...(await orig<Record<string, unknown>>()),
  members: { list, grant, revoke },
  invites: { list: inviteList, create: inviteCreate, revoke: inviteRevoke },
}));

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

const roster = (): MemberList => ({
  creator: "u-creator",
  creator_email: "founder@example.com",
  members: [
    { user_id: "u-prompter", role: "prompter", invited_by: "u-creator", created_at: "2026-09-01T00:00:00Z", updated_at: "2026-09-01T00:00:00Z", email: "prompter@example.com" },
    { user_id: "u-guest", role: "guest", invited_by: "u-creator", created_at: "2026-09-01T00:00:00Z", updated_at: "2026-09-01T00:00:00Z" },
  ],
});

const invite = (over: Partial<InviteInfo> = {}): InviteInfo => ({
  id: "inv-1",
  project_id: "p1",
  role: "prompter",
  email: null,
  created_by: "u-creator",
  created_at: "2026-09-01T00:00:00Z",
  expires_at: "2026-09-08T00:00:00Z",
  max_uses: 1,
  uses: 0,
  revoked_at: null,
  ...over,
});

beforeEach(() => {
  list.mockReset().mockResolvedValue(roster());
  grant.mockReset().mockResolvedValue({});
  revoke.mockReset().mockResolvedValue(undefined);
  inviteList.mockReset().mockResolvedValue([]);
  inviteCreate.mockReset();
  inviteRevoke.mockReset().mockResolvedValue(undefined);
});

function renderPanel(tier: Tier | undefined) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={client}>
      <MembersPanel open onClose={() => {}} projectId="p1" tier={tier} />
    </QueryClientProvider>,
  );
}

describe("the roster — visible to anyone the dialog is open for", () => {
  it("shows the creator and every member, tier included", async () => {
    renderPanel("guest");
    await waitFor(() => expect(screen.queryByTestId("member-creator")).not.toBeNull());
    expect(screen.getByTestId("member-creator").textContent).toMatch(/founder@example.com/);
    expect(screen.getByTestId("member-u-prompter").textContent).toMatch(/Prompter/);
    expect(screen.getByTestId("member-u-guest").textContent).toMatch(/Guest/);
  });

  it("never renders a mutation control for a caller below admin — the API decides, this only offers", async () => {
    renderPanel("prompter");
    await waitFor(() => expect(screen.queryByTestId("member-list")).not.toBeNull());
    expect(screen.queryByTestId("form-add-member")).toBeNull();
    expect(screen.queryByTestId("btn-create-invite")).toBeNull();
    expect(screen.queryByTestId("select-tier-u-prompter")).toBeNull();
    expect(screen.queryByTestId("btn-remove-u-prompter")).toBeNull();
  });

  it("never offers to change or remove the creator, even for an admin caller", async () => {
    renderPanel("admin");
    await waitFor(() => expect(screen.queryByTestId("member-creator")).not.toBeNull());
    expect(screen.getByTestId("member-creator").textContent).toMatch(/Creator/);
    expect(screen.queryByTestId("select-tier-u-creator")).toBeNull();
    expect(screen.queryByTestId("btn-remove-u-creator")).toBeNull();
  });
});

describe("admin controls", () => {
  it("adds a member by account id at the chosen tier", async () => {
    renderPanel("admin");
    await waitFor(() => expect(screen.queryByTestId("form-add-member")).not.toBeNull());
    fireEvent.change(screen.getByTestId("input-add-user-id"), { target: { value: "u-new" } });
    fireEvent.change(screen.getByTestId("select-add-tier"), { target: { value: "guest" } });
    fireEvent.click(screen.getByTestId("btn-add-member"));
    await waitFor(() => expect(grant).toHaveBeenCalledWith("p1", "u-new", "guest"));
  });

  it("changing a member's row tier calls the same grant route — it is an upsert, not a second endpoint", async () => {
    renderPanel("admin");
    await waitFor(() => expect(screen.queryByTestId("select-tier-u-prompter")).not.toBeNull());
    fireEvent.change(screen.getByTestId("select-tier-u-prompter"), { target: { value: "admin" } });
    await waitFor(() => expect(grant).toHaveBeenCalledWith("p1", "u-prompter", "admin"));
  });

  it("removes a member", async () => {
    renderPanel("admin");
    await waitFor(() => expect(screen.queryByTestId("btn-remove-u-guest")).not.toBeNull());
    fireEvent.click(screen.getByTestId("btn-remove-u-guest"));
    await waitFor(() => expect(revoke).toHaveBeenCalledWith("p1", "u-guest"));
  });
});

describe("invites", () => {
  it("creates one and shows the token exactly once — the server's own guarantee", async () => {
    const created: CreatedInvite = { ...invite({ email: "souren@example.com" }), token: "wi_abcDEF123" };
    inviteCreate.mockResolvedValue(created);
    renderPanel("admin");
    await waitFor(() => expect(screen.queryByTestId("btn-create-invite")).not.toBeNull());
    fireEvent.change(screen.getByTestId("input-invite-email"), { target: { value: "souren@example.com" } });
    fireEvent.click(screen.getByTestId("btn-create-invite"));
    await waitFor(() =>
      expect(inviteCreate).toHaveBeenCalledWith("p1", { role: "prompter", email: "souren@example.com" }),
    );
    await waitFor(() => expect(screen.getByTestId("invite-token").textContent).toBe("wi_abcDEF123"));
  });

  it("offers a ready-to-send link built from the page's own origin, with the raw token secondary", async () => {
    vi.stubGlobal("location", { ...window.location, origin: "https://wheel.example" });
    const writeText = vi.fn(async () => undefined);
    vi.stubGlobal("navigator", { ...navigator, clipboard: { writeText } });
    inviteCreate.mockResolvedValue({ ...invite(), token: "wi_abcDEF123" });
    renderPanel("admin");
    await waitFor(() => expect(screen.queryByTestId("btn-create-invite")).not.toBeNull());
    fireEvent.click(screen.getByTestId("btn-create-invite"));
    const link = await screen.findByTestId("invite-link");
    expect(link.textContent).toBe("https://wheel.example/app/invite/wi_abcDEF123");
    expect(screen.getByTestId("invite-token").textContent).toBe("wi_abcDEF123");
    expect(link.compareDocumentPosition(screen.getByTestId("invite-token")) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    fireEvent.click(link.parentElement!.querySelector("button")!);
    await waitFor(() => expect(writeText).toHaveBeenCalledWith("https://wheel.example/app/invite/wi_abcDEF123"));
  });

  it("omits the email field entirely when none was typed, rather than sending an empty string", async () => {
    inviteCreate.mockResolvedValue({ ...invite(), token: "wi_xyz" });
    renderPanel("admin");
    await waitFor(() => expect(screen.queryByTestId("btn-create-invite")).not.toBeNull());
    fireEvent.click(screen.getByTestId("btn-create-invite"));
    await waitFor(() => expect(inviteCreate).toHaveBeenCalledWith("p1", { role: "prompter" }));
  });

  it("lists a pending invite and can revoke it", async () => {
    inviteList.mockResolvedValue([invite({ email: "souren@example.com", uses: 0, max_uses: 1 })]);
    renderPanel("admin");
    await waitFor(() => expect(screen.queryByTestId("invite-inv-1")).not.toBeNull());
    expect(screen.getByTestId("invite-inv-1").textContent).toMatch(/souren@example.com/);
    fireEvent.click(screen.getByTestId("btn-revoke-invite-inv-1"));
    await waitFor(() => expect(inviteRevoke).toHaveBeenCalledWith("p1", "inv-1"));
  });
});
