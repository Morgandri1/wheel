import { describe, expect, it } from "vitest";
import {
  buildBudget,
  buildIdleTimeout,
  buildWorkspace,
  parseOptionalNumber,
  validateWorkspacePath,
} from "./agent-config";

describe("parseOptionalNumber — empty is unset, not zero", () => {
  it("treats an empty field as unset", () => {
    // The trap: "" -> 0 turns "I left this blank" into "spend nothing", which stops the agent on
    // its first turn while looking like a deliberate budget.
    expect(parseOptionalNumber("")).toBeUndefined();
    expect(parseOptionalNumber("   ")).toBeUndefined();
  });

  it("keeps an explicit zero, which is a real and different instruction", () => {
    expect(parseOptionalNumber("0")).toBe(0);
  });

  it("rejects nonsense and negatives rather than coercing them", () => {
    for (const bad of ["abc", "-1", "1e", "NaN"]) expect(parseOptionalNumber(bad)).toBeNull();
  });
});

describe("buildBudget", () => {
  it("drops the budget entirely when both fields are blank", () => {
    expect(buildBudget("", "")).toEqual({ ok: true, budget: null });
  });

  it("keeps only the field that was filled", () => {
    expect(buildBudget("40", "")).toEqual({ ok: true, budget: { max_turns: 40 } });
    expect(buildBudget("", "2.50")).toEqual({ ok: true, budget: { max_usd: 2.5 } });
  });

  it("floors turns, which are whole, and leaves dollars fractional", () => {
    expect(buildBudget("40.7", "2.55")).toEqual({ ok: true, budget: { max_turns: 40, max_usd: 2.55 } });
  });

  it("reports rejection instead of saving a partial budget", () => {
    expect(buildBudget("abc", "1")).toEqual({ ok: false, message: expect.stringContaining("number") });
  });
});

describe("validateWorkspacePath", () => {
  it("refuses what the engine would refuse", () => {
    expect(validateWorkspacePath("/etc/passwd")).toMatch(/absolute/i);
    expect(validateWorkspacePath("../../secrets")).toMatch(/\.\./);
    expect(validateWorkspacePath("")).toMatch(/needs a path/i);
    expect(validateWorkspacePath("a b;rm -rf")).toMatch(/letters/i);
  });

  it("accepts an ordinary relative path", () => {
    expect(validateWorkspacePath("repos/wheel")).toBeNull();
    expect(validateWorkspacePath("wheel")).toBeNull();
  });

  it("does not mistake a dotfile or a hyphen for traversal", () => {
    expect(validateWorkspacePath(".config/thing")).toBeNull();
    expect(validateWorkspacePath("my-repo/sub_dir")).toBeNull();
  });
});

describe("buildWorkspace", () => {
  it("omits git entirely rather than storing an empty block", () => {
    expect(buildWorkspace("repos/wheel", "", "")).toEqual({ path: "repos/wheel" });
  });

  it("keeps url alone, and url+ref when both given", () => {
    expect(buildWorkspace("w", "https://x/y.git", "")).toEqual({ path: "w", git: { url: "https://x/y.git" } });
    expect(buildWorkspace("w", "https://x/y.git", "main")).toEqual({
      path: "w",
      git: { url: "https://x/y.git", ref: "main" },
    });
  });
});

describe("the write must carry the whole config", () => {
  /**
   * The trap this guards: PATCH replaces config wholesale, and `Partial<Config>` type-checks a
   * one-field object happily. Sending {budget} alone would delete system_prompt and harness — the
   * agent would come back unstartable, and nothing would have reported an error.
   */
  it("a config patch spread keeps every sibling field", () => {
    const config = {
      harness: "claude",
      system_prompt: "you are a researcher",
      run_on_startup: true,
      workspaces: [{ path: "repos/wheel" }],
    };
    const parsed = buildBudget("40", "");
    const sent = { ...config, budget: parsed.ok ? parsed.budget : undefined };
    expect(sent.system_prompt).toBe("you are a researcher");
    expect(sent.harness).toBe("claude");
    expect(sent.workspaces).toEqual([{ path: "repos/wheel" }]);
    expect(sent.budget).toEqual({ max_turns: 40 });
  });

  it("removing a workspace keeps the others and changes nothing else", () => {
    const workspaces = [{ path: "a" }, { path: "b" }, { path: "c" }];
    expect(workspaces.filter((_, i) => i !== 1)).toEqual([{ path: "a" }, { path: "c" }]);
  });
});


describe("clearing a field survives JSON, under merge OR replace semantics", () => {
  /**
   * JSON.stringify DROPS undefined keys. `{...config, budget: undefined}` serialises with no
   * `budget` at all — which clears it under replace semantics and, under merge, means "leave it
   * alone", so a user clearing the box would keep their cap and be told it saved. An explicit null
   * means unset under both. This asserts the SERIALISED body, because that is where undefined
   * disappears and an object-level assertion would not notice.
   */
  it("sends an explicit null for a cleared budget", () => {
    const parsed = buildBudget("", "");
    expect(parsed).toEqual({ ok: true, budget: null });
    const body = JSON.stringify({ config: { harness: "claude", budget: parsed.ok ? parsed.budget : undefined } });
    expect(body).toContain('"budget":null');
    expect(JSON.parse(body).config).toHaveProperty("budget");
  });

  it("sends an explicit null for a cleared idle timeout", () => {
    const parsed = buildIdleTimeout("  ");
    expect(parsed).toEqual({ ok: true, secs: null });
    const body = JSON.stringify({ config: { idle_timeout_secs: parsed.ok ? parsed.secs : undefined } });
    expect(body).toContain('"idle_timeout_secs":null');
  });

  it("still keeps a real value, and floors the seconds", () => {
    expect(buildIdleTimeout("90.7")).toEqual({ ok: true, secs: 90 });
    expect(buildIdleTimeout("0")).toEqual({ ok: true, secs: 0 });
    expect(buildIdleTimeout("nope")).toEqual({ ok: false, message: expect.stringContaining("seconds") });
  });
});
