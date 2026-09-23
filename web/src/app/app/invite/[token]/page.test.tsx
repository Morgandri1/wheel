// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import { afterEach, describe, expect, it, vi } from "vitest";
import { Suspense } from "react";
import { act, cleanup, render, screen, waitFor } from "@testing-library/react";

const router = vi.hoisted(() => ({ replace: vi.fn(), push: vi.fn() }));
vi.mock("next/navigation", () => ({ useRouter: () => router }));
vi.mock("@/components/header", () => ({ Header: () => null }));

import AcceptInvitePage from "./page";

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  router.replace.mockReset();
  router.push.mockReset();
});

async function renderPage(answer: Response | Error) {
  const fetchMock = vi.fn(async () => {
    if (answer instanceof Error) throw answer;
    return answer;
  });
  vi.stubGlobal("fetch", fetchMock);
  const params = Promise.resolve({ token: "wi_abc" });
  await act(async () => {
    render(
      <Suspense>
        <AcceptInvitePage params={params} />
      </Suspense>,
    );
  });
  return fetchMock;
}

describe("the invite page", () => {
  it("redeems once and lands on the project", async () => {
    const fetchMock = await renderPage(Response.json({ project_id: "p1", role: "prompter" }));
    await waitFor(() => expect(router.replace).toHaveBeenCalledWith("/app/p1"));
    expect(fetchMock).toHaveBeenCalledTimes(1);
  });

  it("shows the API's plain message for an unusable invite, not a generic error", async () => {
    await renderPage(
      Response.json(
        { error: { code: "invite_unusable", message: "This invite link cannot be used. It may be expired." } },
        { status: 404 },
      ),
    );
    const box = await screen.findByTestId("invite-error");
    expect(box.textContent).toContain("This invite link cannot be used. It may be expired.");
    expect(box.textContent).not.toContain("gone, or was never yours");
    expect(router.replace).not.toHaveBeenCalled();
  });

  it("says the server is unreachable when the request never lands", async () => {
    await renderPage(new TypeError("fetch failed"));
    expect((await screen.findByTestId("invite-error")).textContent).toContain("Check your connection");
  });
});
