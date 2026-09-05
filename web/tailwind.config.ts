import type { Config } from "tailwindcss";

/**
 * "Patchbay" — see docs/plans/web.md §1.
 * Every colour is a CSS variable so the light theme is a real second theme,
 * not an inverted afterthought. Accents are the wire code and appear ONLY on
 * wires, the wire legend, and the affordances that create wires.
 */
const config: Config = {
  darkMode: ["class", '[data-theme="dark"]'],
  content: ["./src/**/*.{ts,tsx}"],
  theme: {
    extend: {
      colors: {
        panel: {
          0: "var(--panel-0)",
          1: "var(--panel-1)",
          2: "var(--panel-2)",
        },
        rule: "var(--rule)",
        "rule-strong": "var(--rule-strong)",
        ink: "var(--ink)",
        "ink-dim": "var(--ink-dim)",
        "ink-faint": "var(--ink-faint)",
        live: "var(--live)",
        alarm: "var(--alarm)",
        wire: {
          read: "var(--wire-read)",
          write: "var(--wire-write)",
          send: "var(--wire-send)",
        },
      },
      fontFamily: {
        sans: ["var(--font-archivo)", "ui-sans-serif", "system-ui", "sans-serif"],
        display: ["var(--font-archivo-expanded)", "var(--font-archivo)", "sans-serif"],
        mono: ["var(--font-mono)", "ui-monospace", "SFMono-Regular", "monospace"],
      },
      fontSize: {
        // 12 / 13 / 15 / 18 / 24 / 34 / 52 — docs/plans/web.md §1
        micro: ["0.75rem", { lineHeight: "1rem" }],
        small: ["0.8125rem", { lineHeight: "1.15rem" }],
        base: ["0.9375rem", { lineHeight: "1.55" }],
        lead: ["1.125rem", { lineHeight: "1.5" }],
        title: ["1.5rem", { lineHeight: "1.25", letterSpacing: "-0.01em" }],
        display: ["2.125rem", { lineHeight: "1.1", letterSpacing: "-0.02em" }],
        hero: ["3.25rem", { lineHeight: "1.02", letterSpacing: "-0.03em" }],
      },
      borderRadius: {
        plate: "2px",
        control: "3px",
      },
      boxShadow: {
        // The only shadow in the system: a 1px milled-edge highlight.
        plate: "inset 0 1px 0 0 var(--edge-hi)",
      },
      transitionDuration: { fast: "110ms" },
    },
  },
  plugins: [],
};

export default config;
