import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";

vi.mock("next/navigation", () => ({
  useRouter: () => ({ replace: vi.fn(), push: vi.fn() }),
  useSearchParams: () => new URLSearchParams(),
}));

/**
 * The sign-in form is the one place in the app where a user types a secret, so the only thing
 * tested here is that the secret cannot escape into a URL.
 *
 * AUTH_MODE is read at module load, so the module is imported fresh with the env stubbed —
 * imported at the top it renders the "not in local mode" notice and there is no form to assert on.
 */
async function renderScreen(mode: "sign-in" | "sign-up") {
  vi.stubEnv("NEXT_PUBLIC_AUTH_MODE", "local");
  vi.resetModules();
  const { AuthScreen } = await import("@/components/auth/auth-screen");
  return render(<AuthScreen mode={mode} />);
}

afterEach(() => {
  cleanup();
  vi.unstubAllEnvs();
});

/**
 * Every <form> in web/, not just this one. A GET form serialises its NAMED fields into the URL on
 * any submission we do not intercept — and "no field is named today" is one password-manager
 * attribute away from being false. Pinning the rule for all forms is what stops the next one
 * being written the leaky way.
 */
describe("no form in web/ is a GET form", () => {
  it("declares method=post everywhere a <form> is used", async () => {
    const { readFileSync, readdirSync, statSync } = await import("node:fs");
    const { join } = await import("node:path");
    const files: string[] = [];
    const walk = (dir: string) => {
      for (const entry of readdirSync(dir)) {
        const full = join(dir, entry);
        if (statSync(full).isDirectory()) walk(full);
        else if (full.endsWith(".tsx") && !full.endsWith(".test.tsx")) files.push(full);
      }
    };
    walk("src");

    const offenders: string[] = [];
    for (const file of files) {
      const text = readFileSync(file, "utf8");
      for (const match of text.matchAll(/<form\b([\s\S]{0,400}?)>/g)) {
        if (!/method=["{]?["']?post/i.test(match[1]!)) offenders.push(file);
      }
    }
    expect(offenders).toEqual([]);
  });
});

describe("the auth form cannot leak credentials into a URL", () => {
  // Production bug: a click before React hydrated ran the BROWSER's submission rather than ours.
  // A form with no method defaults to GET, which serialises every named field into the query
  // string — so the password landed in the URL, and from there in history, the Referer header and
  // the CDN's access logs. preventDefault could not help: its handler was not attached yet.
  it("declares POST, so a submission we do not control cannot put fields in the URL", async () => {
    const { container } = await renderScreen("sign-in");
    const form = container.querySelector("form");
    expect(form).not.toBeNull();
    expect(form!.getAttribute("method")?.toLowerCase()).toBe("post");
  });

  it("is never a GET form, which is the shape that leaks", async () => {
    const { container } = await renderScreen("sign-up");
    expect(container.querySelector("form")!.getAttribute("method")?.toLowerCase()).not.toBe("get");
  });

  // The hydration guard must not be implemented as a button that is disabled forever.
  it("offers a usable submit button once hydrated", async () => {
    await renderScreen("sign-in");
    expect((screen.getByTestId("btn-auth-submit") as HTMLButtonElement).disabled).toBe(false);
  });
});
