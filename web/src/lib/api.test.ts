// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { ApiError, applyBoard, engineApi, projects } from "./api";

/**
 * The client wiring for `api-paths`: an id taken from the route never shapes the path, and a
 * refused id never reaches fetch at all — it answers like the 404 it would have been.
 */
const ID = "0d9f2c1e-7b4a-4c3d-9e8f-1a2b3c4d5e6f";
let fetchMock: ReturnType<typeof vi.fn>;

beforeEach(() => {
  fetchMock = vi.fn(async () => new Response("{}", { headers: { "content-type": "application/json" } }));
  vi.stubGlobal("fetch", fetchMock);
});

afterEach(() => vi.unstubAllGlobals());

const urlOf = (i = 0) => (fetchMock.mock.calls[i] as [string])[0];

describe("a project id from the route", () => {
  it.each([
    ["the board", () => engineApi("../../auth/me").board()],
    ["a project", () => projects.get("p1")],
    ["a lifecycle action", () => projects.start("x/../../auth")],
    ["board apply", () => applyBoard("%2e%2e", {}, true)],
  ])("that is not a UUID never reaches fetch for %s", async (_label, call) => {
    const refusal = await call().catch((e: unknown) => e);
    expect(refusal).toBeInstanceOf(ApiError);
    expect((refusal as ApiError).status).toBe(404);
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it("that is a UUID reaches the proxy as one segment", async () => {
    await engineApi(ID).board();
    expect(urlOf()).toBe(`/api/wheel/v1/projects/${ID}/engine/v1/board`);
  });
});

describe("other ids", () => {
  it("are encoded as a single segment", async () => {
    await engineApi(ID).deleteNode("a/../b");
    expect(urlOf()).toBe(`/api/wheel/v1/projects/${ID}/engine/v1/nodes/a%2F..%2Fb`);
  });

  it("go in the query, encoded, where the engine expects them there", async () => {
    await engineApi(ID).chest("c1").remove("dir/a b.txt");
    expect(urlOf()).toBe(`/api/wheel/v1/projects/${ID}/engine/v1/chests/c1/blob?key=dir%2Fa+b.txt`);
  });

  it("keep the vault key a single segment", async () => {
    await engineApi(ID).putSecret("v1", "ANTHROPIC_API_KEY", "sk");
    expect(urlOf()).toBe(`/api/wheel/v1/projects/${ID}/engine/v1/vault/v1/ANTHROPIC_API_KEY`);
  });
});
