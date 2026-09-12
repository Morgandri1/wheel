// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";

const turns = vi.fn();
const put = vi.fn();
vi.mock("@/lib/builder-client", async () => {
  const actual = await vi.importActual<typeof import("@/lib/builder-client")>("@/lib/builder-client");
  return {
    ...actual,
    builderTurns: (...args: unknown[]) => turns(...args),
    builderCredential: { get: vi.fn(), put: (...args: unknown[]) => put(...args), remove: vi.fn() },
  };
});
vi.mock("@/lib/api", () => ({ applyBoard: vi.fn(async () => ({ kind: "plan", plan: {}, digest: null })) }));

import { BuilderStopped } from "@/lib/builder-client";
import { BuilderSession } from "./builder-session";

const PROJECT = "11111111-1111-4111-8111-111111111111";

afterEach(() => {
  cleanup();
  turns.mockReset();
  put.mockReset();
});

/** A run that refuses before it starts, the way the engine answers a project with no credential. */
function refusesWith(refusal: { code: string; message: string; sources?: unknown }) {
  turns.mockImplementation(async function* () {
    throw new BuilderStopped({ status: 409, ...refusal } as never);
  });
}

async function ask(known: { id: string; name: string; type: "agent" | "vault" | "ctx" }[] = []) {
  render(<BuilderSession projectId={PROJECT} mode="new" known={known} />);
  fireEvent.change(screen.getByTestId("builder-input"), { target: { value: "a researcher" } });
  fireEvent.click(screen.getByTestId("btn-builder-send"));
}

describe("a builder that cannot run without a credential", () => {
  /**
   * Retrying cannot fix a project with no credential, and the builder spends the user's own. So
   * the refusal has to become a choice rather than an error the user reads twice.
   */
  it("turns needs_auth into the choice the user can actually make", async () => {
    refusesWith({
      code: "needs_auth",
      message: "the builder has no credential in this project",
      sources: { agents: [{ id: "a1", name: "worker" }], vaults: [{ id: "v1", name: "secrets" }] },
    });
    await ask();

    await waitFor(() => expect(screen.queryByTestId("builder-needs-auth")).not.toBeNull());
    expect(screen.getByTestId("builder-needs-auth").textContent).toMatch(/no credential/);
    // Both kinds of source the engine offered are choosable.
    const options = [...screen.getByTestId("select-builder-credential").querySelectorAll("option")].map(
      (o) => o.textContent,
    );
    expect(options).toContain("worker (agent)");
    expect(options).toContain("secrets (vault)");
  });

  it("runs the next turn on the source the user picked", async () => {
    refusesWith({
      code: "needs_auth",
      message: "no credential",
      sources: { agents: [{ id: "a1", name: "worker" }], vaults: [] },
    });
    await ask();
    await waitFor(() => expect(screen.queryByTestId("builder-needs-auth")).not.toBeNull());

    fireEvent.change(screen.getByTestId("select-builder-credential"), { target: { value: "agent:a1" } });
    // Choosing answers the question, so the prompt goes away.
    await waitFor(() => expect(screen.queryByTestId("builder-needs-auth")).toBeNull());

    turns.mockImplementation(async function* () {
      yield { kind: "delta", text: "right, a researcher" };
      yield { kind: "done", text: "right, a researcher", boards: 0 };
    });
    fireEvent.change(screen.getByTestId("builder-input"), { target: { value: "go on" } });
    fireEvent.click(screen.getByTestId("btn-builder-send"));

    await waitFor(() => expect(turns).toHaveBeenCalledTimes(2));
    expect(turns.mock.calls[1]?.[1]).toMatchObject({ credential: { source: "agent", node: "a1" } });
  });

  /**
   * A brand-new project has no agent and no vault to point at, which is exactly why the builder
   * can hold one of its own.
   */
  it("takes a credential of the builder's own when there is nothing else to choose", async () => {
    refusesWith({ code: "needs_auth", message: "no credential", sources: { agents: [], vaults: [] } });
    await ask();
    await waitFor(() => expect(screen.queryByTestId("builder-needs-auth")).not.toBeNull());
    expect(screen.queryByTestId("select-builder-credential")).toBeNull();

    put.mockResolvedValue({ configured: true, kind: "oauth_token" });
    fireEvent.change(screen.getByTestId("input-builder-credential"), {
      target: { value: "sk-ant-oat01-durable" },
    });
    fireEvent.click(screen.getByTestId("btn-builder-credential"));

    // Filed as what it is: an `sk-ant-oat…` value is a setup-token, and sending it as an api_key
    // would store it under a variable the harness does not read.
    await waitFor(() => expect(put).toHaveBeenCalledWith(PROJECT, { setup_token: "sk-ant-oat01-durable" }));
    await waitFor(() => expect(screen.queryByTestId("builder-needs-auth")).toBeNull());
  });

  it("files a provider key as a key rather than as a setup token", async () => {
    refusesWith({ code: "needs_auth", message: "no credential", sources: { agents: [], vaults: [] } });
    await ask();
    await waitFor(() => expect(screen.queryByTestId("builder-needs-auth")).not.toBeNull());

    put.mockResolvedValue({ configured: true, kind: "api_key" });
    fireEvent.change(screen.getByTestId("input-builder-credential"), { target: { value: "sk-ant-api03-key" } });
    fireEvent.click(screen.getByTestId("btn-builder-credential"));
    await waitFor(() => expect(put).toHaveBeenCalledWith(PROJECT, { api_key: "sk-ant-api03-key" }));
  });

  it("does not ask about credentials for a failure that is not about one", async () => {
    refusesWith({ code: "builder_busy", message: "a builder turn is already running" });
    await ask();
    await waitFor(() =>
      expect(screen.getByTestId("builder-turn-builder").textContent).toMatch(/already running/),
    );
    expect(screen.queryByTestId("builder-needs-auth")).toBeNull();
  });
});

describe("what the session sends", () => {
  it("carries the mode and the conversation, on the builder's own credential by default", async () => {
    turns.mockImplementation(async function* () {
      yield { kind: "done", text: "hello", boards: 0 };
    });
    render(<BuilderSession projectId={PROJECT} mode="improve" known={[]} />);
    fireEvent.change(screen.getByTestId("builder-input"), { target: { value: "add a table" } });
    fireEvent.click(screen.getByTestId("btn-builder-send"));

    await waitFor(() => expect(turns).toHaveBeenCalled());
    expect(turns.mock.calls[0]?.[0]).toBe(PROJECT);
    expect(turns.mock.calls[0]?.[1]).toMatchObject({
      mode: "improve",
      credential: { source: "builder" },
      turns: [{ role: "user", text: "add a table" }],
    });
  });
});
