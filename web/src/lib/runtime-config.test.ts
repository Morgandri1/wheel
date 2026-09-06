import { afterEach, describe, expect, it, vi } from "vitest";

type Mod = typeof import("./runtime-config");

async function load(env: Record<string, string | undefined>): Promise<Mod> {
  for (const [k, v] of Object.entries(env)) {
    if (v === undefined) vi.stubEnv(k, "");
    else vi.stubEnv(k, v);
  }
  vi.resetModules();
  return (await import("./runtime-config")) as Mod;
}

afterEach(() => vi.unstubAllEnvs());

/**
 * The whole reason this module exists: a prebuilt package cannot read a NEXT_PUBLIC_* value from
 * the user's environment, because that value was inlined when the bundle was compiled. If these
 * tests pass while the resolution silently falls back to the build-time constant, `npx wheel-web`
 * ships with an option that does nothing.
 */
describe("serverApiBaseUrl", () => {
  it("prefers WHEEL_API_URL, the runtime knob", async () => {
    const m = await load({ WHEEL_API_URL: "https://api.example.test", NEXT_PUBLIC_API_URL: "https://baked.test" });
    expect(m.serverApiBaseUrl()).toBe("https://api.example.test");
  });

  it("falls back to the build-time value a self-built deployment sets", async () => {
    const m = await load({ WHEEL_API_URL: undefined, NEXT_PUBLIC_API_URL: "https://baked.test" });
    expect(m.serverApiBaseUrl()).toBe("https://baked.test");
  });

  it("strips a trailing slash, so paths never double up", async () => {
    const m = await load({ WHEEL_API_URL: "https://api.example.test/" });
    expect(m.serverApiBaseUrl()).toBe("https://api.example.test");
  });
});

describe("the client's view", () => {
  it("uses the build-time value until the server tells it otherwise", async () => {
    const m = await load({ WHEEL_API_URL: undefined, NEXT_PUBLIC_API_URL: "https://baked.test" });
    expect(m.apiBaseUrl()).toBe("https://baked.test");
  });

  it("takes the runtime value once recorded", async () => {
    const m = await load({ NEXT_PUBLIC_API_URL: "https://baked.test" });
    m.setApiBaseUrl("https://runtime.test/");
    expect(m.apiBaseUrl()).toBe("https://runtime.test");
  });

  // A blank or missing value must not wipe out a working URL: the fallback is the last thing
  // standing between the app and every request going to the wrong origin.
  it.each(["", "   ", null, undefined])("ignores %o rather than blanking the URL", async (bad) => {
    const m = await load({ NEXT_PUBLIC_API_URL: "https://baked.test" });
    m.setApiBaseUrl(bad as string | null | undefined);
    expect(m.apiBaseUrl()).toBe("https://baked.test");
  });
});
