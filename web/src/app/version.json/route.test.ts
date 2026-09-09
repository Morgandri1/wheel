// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import { describe, expect, it } from "vitest";
import { GET } from "./route";
import pkg from "../../../package.json";

describe("/version.json — the deploy is checkable without logging in", () => {
  /**
   * This route exists because every feature ships behind the login wall, so "which build is live"
   * had no answer from outside. If it ever stops reporting the real version, the answer silently
   * becomes wrong rather than absent — which is the failure it was built to end.
   */
  it("reports the version the package actually declares", async () => {
    const body = await GET().json();
    expect(body.version).toBe(pkg.version);
    expect(body.version).toMatch(/^\d+\.\d+\.\d+$/);
  });

  it("is never cached, because a stale answer looks exactly like a correct one", () => {
    expect(GET().headers.get("cache-control")).toBe("no-store");
  });

  it("names the commit when the build knows it, and null rather than a guess when it does not", async () => {
    const body = await GET().json();
    expect(body).toHaveProperty("commit");
    expect(body.commit === null || typeof body.commit === "string").toBe(true);
  });
});
