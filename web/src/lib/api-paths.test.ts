// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import { describe, expect, it } from "vitest";
import { isProjectId, projectPath, withQuery } from "./api-paths";

const ID = "0d9f2c1e-7b4a-4c3d-9e8f-1a2b3c4d5e6f";

describe("which project ids become a path", () => {
  it("accepts a UUID, in either case", () => {
    expect(isProjectId(ID)).toBe(true);
    expect(isProjectId(ID.toUpperCase())).toBe(true);
  });

  it.each([
    "p1",
    "",
    "../../auth/me",
    "%2e%2e",
    `${ID}/../../auth`,
    `${ID}?x=1`,
    `${ID}#x`,
    ` ${ID}`,
    `${ID}0`,
    "0d9f2c1e7b4a4c3d9e8f1a2b3c4d5e6f",
  ])("refuses %j before it reaches a path", (id) => {
    expect(isProjectId(id)).toBe(false);
    expect(projectPath(id, "engine", "v1", "board")).toBeNull();
  });
});

describe("building the path", () => {
  it("puts the project first and every segment after it", () => {
    expect(projectPath(ID)).toBe(`/v1/projects/${ID}`);
    expect(projectPath(ID, "engine", "v1", "board")).toBe(`/v1/projects/${ID}/engine/v1/board`);
  });

  it.each([
    ["a slash", "a/b", "a%2Fb"],
    ["a query", "a?b", "a%3Fb"],
    ["a fragment", "a#b", "a%23b"],
    ["a backslash", "a\\b", "a%5Cb"],
    ["a percent sign", "100%", "100%25"],
    ["a space", "a b", "a%20b"],
    ["an ordinary id", "ANTHROPIC_API_KEY", "ANTHROPIC_API_KEY"],
  ])("encodes %s inside one segment", (_label, id, encoded) => {
    expect(projectPath(ID, "engine", "v1", "nodes", id)).toBe(`/v1/projects/${ID}/engine/v1/nodes/${encoded}`);
  });

  // A browser resolves dot segments, even percent-encoded ones, before the request leaves, so
  // encoding cannot protect them. They are refused instead.
  it.each(["", ".", ".."])("refuses the segment %j, which encoding cannot protect", (segment) => {
    expect(projectPath(ID, "engine", "v1", "nodes", segment)).toBeNull();
  });
});

describe("queries", () => {
  it("encodes values and appends them", () => {
    expect(withQuery(`/v1/projects/${ID}/engine/v1/chests/c/blob`, new URLSearchParams({ key: "dir/a b&c" }))).toBe(
      `/v1/projects/${ID}/engine/v1/chests/c/blob?key=dir%2Fa+b%26c`,
    );
  });

  it("adds nothing for no parameters", () => {
    expect(withQuery("/v1/projects", new URLSearchParams())).toBe("/v1/projects");
  });

  it("keeps a refused path refused", () => {
    expect(withQuery(null, new URLSearchParams({ a: "1" }))).toBeNull();
  });
});
