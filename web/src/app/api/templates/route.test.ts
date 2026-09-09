import { afterEach, describe, expect, it, vi } from "vitest";

const readdir = vi.hoisted(() => vi.fn());
const readFile = vi.hoisted(() => vi.fn());
vi.mock("node:fs/promises", () => ({ readdir, readFile, default: { readdir, readFile } }));

afterEach(() => {
  vi.restoreAllMocks();
  readdir.mockReset();
  readFile.mockReset();
});

describe("GET /api/templates — reads public/workflow_templates at request time", () => {
  it("reports an empty gallery, not an error, when the directory does not exist", async () => {
    readdir.mockRejectedValue(Object.assign(new Error("no such dir"), { code: "ENOENT" }));
    const { GET } = await import("./route");
    const body = await GET().then((r) => r.json());
    expect(body).toEqual({ templates: [] });
  });

  it("lists a valid template with its summary fields, not the full board", async () => {
    readdir.mockResolvedValue(["research-crew.json"]);
    readFile.mockResolvedValue(
      JSON.stringify({
        title: "Research crew",
        description: "An agent and its notes.",
        requires_capabilities: { http: false },
        board: {
          nodes: [
            { name: "notes", type: "ctx", config: { markdown: "x" } },
            { name: "researcher", type: "agent", config: { harness: "claude", system_prompt: "p" } },
          ],
          wires: [{ from: "notes", to: "researcher", type: "send" }],
        },
      }),
    );
    const { GET } = await import("./route");
    const body = await GET().then((r) => r.json());
    expect(body.templates).toEqual([
      {
        slug: "research-crew",
        title: "Research crew",
        description: "An agent and its notes.",
        nodeCount: 2,
        wireCount: 1,
        warnings: [],
        requiresCapabilities: { http: false },
      },
    ]);
    // The board itself never leaves this route — a card is not the wire it would create.
    expect(JSON.stringify(body)).not.toContain("markdown");
  });

  it("skips a malformed file rather than 500ing the whole gallery", async () => {
    readdir.mockResolvedValue(["broken.json", "ok.json"]);
    readFile.mockImplementation((p: string) => {
      if (p.endsWith("broken.json")) return Promise.resolve("{ not json");
      return Promise.resolve(
        JSON.stringify({
          title: "OK",
          description: "d",
          board: { nodes: [{ name: "a", type: "ctx", config: { markdown: "x" } }], wires: [] },
        }),
      );
    });
    const errorSpy = vi.spyOn(console, "error").mockImplementation(() => {});
    const { GET } = await import("./route");
    const body = await GET().then((r) => r.json());
    expect(body.templates).toHaveLength(1);
    expect(body.templates[0].slug).toBe("ok");
    expect(errorSpy).toHaveBeenCalled();
  });

  it("skips a file that parses as JSON but fails template validation", async () => {
    readdir.mockResolvedValue(["invalid.json"]);
    readFile.mockResolvedValue(JSON.stringify({ title: "", description: "", board: { nodes: [], wires: [] } }));
    const errorSpy = vi.spyOn(console, "error").mockImplementation(() => {});
    const { GET } = await import("./route");
    const body = await GET().then((r) => r.json());
    expect(body.templates).toEqual([]);
    expect(errorSpy).toHaveBeenCalledWith(expect.stringContaining("invalid.json"));
  });

  it("ignores non-json files in the directory", async () => {
    readdir.mockResolvedValue(["README.md", "research-crew.json"]);
    readFile.mockResolvedValue(
      JSON.stringify({ title: "t", description: "d", board: { nodes: [], wires: [] } }),
    );
    const { GET } = await import("./route");
    const body = await GET().then((r) => r.json());
    expect(readFile).toHaveBeenCalledTimes(1);
    expect(body.templates).toHaveLength(1);
  });

  it("sorts the list alphabetically by slug, not directory order", async () => {
    readdir.mockResolvedValue(["zeta.json", "alpha.json"]);
    readFile.mockImplementation((p: string) =>
      Promise.resolve(
        JSON.stringify({
          title: p.includes("zeta") ? "Zeta" : "Alpha",
          description: "d",
          board: { nodes: [], wires: [] },
        }),
      ),
    );
    const { GET } = await import("./route");
    const body = await GET().then((r) => r.json());
    expect(body.templates.map((t: { slug: string }) => t.slug)).toEqual(["alpha", "zeta"]);
  });
});

describe("GET /api/templates — against the real shipped templates", () => {
  it("lists both real templates with no validation errors", async () => {
    vi.doUnmock("node:fs/promises");
    vi.resetModules();
    const { GET } = await import("./route");
    const body = await GET().then((r) => r.json());
    const slugs = body.templates.map((t: { slug: string }) => t.slug).sort();
    expect(slugs).toEqual(["research-crew", "webhook-logger"]);
  });
});
