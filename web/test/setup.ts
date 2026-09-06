import { JSDOM } from "jsdom";

/**
 * Give the tests a real `localStorage` on Node ≥ 22.
 *
 * Node ships its own Web Storage global, and it is defined on `globalThis` as a NON-ENUMERABLE
 * getter that returns undefined unless the process was started with `--localstorage-file`.
 * Vitest's jsdom environment populates globals by copying the jsdom window's own enumerable
 * properties onto `globalThis` (which it also makes `window`), so Node's pre-existing definition
 * survives and shadows jsdom's. `sessionStorage` comes through fine; only `localStorage` is
 * shadowed, which is why the failure looks so arbitrary.
 *
 * The result was 31 failures on Node 26 and none on CI's Node 22 — a suite that disagrees with
 * itself depending on the machine, which is worse than a red one.
 *
 * This borrows the Storage from a throwaway jsdom window rather than hand-rolling a fake, so the
 * tests exercise the same implementation CI does. It is a no-op wherever jsdom's own already won.
 */
if (typeof globalThis.localStorage === "undefined") {
  const { window } = new JSDOM("", { url: "http://localhost/" });
  Object.defineProperty(globalThis, "localStorage", {
    value: window.localStorage,
    configurable: true,
    writable: true,
  });
}
