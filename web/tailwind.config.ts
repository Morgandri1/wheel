import type { Config } from "tailwindcss";

/**
 * "Patchbay": milled panel plates, hairline structure, and colour reserved for the wire code.
 * Every value here is a token from docs/plans/web.md §1 — nothing ad hoc in components.
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
        danger: "var(--danger)",
        accent: "var(--accent)",
        "accent-deep": "var(--accent-deep)",
        read: "var(--wire-read)",
        write: "var(--wire-write)",
        send: "var(--wire-send)",
      },
      fontFamily: {
        sans: ["var(--font-archivo)", "ui-sans-serif", "system-ui", "sans-serif"],
        mono: ["var(--font-mono)", "ui-monospace", "SFMono-Regular", "monospace"],
      },
      fontSize: {
        micro: ["0.75rem", { lineHeight: "1.1rem" }],
        meta: ["0.8125rem", { lineHeight: "1.2rem" }],
        base: ["0.9375rem", { lineHeight: "1.55" }],
        lead: ["1.125rem", { lineHeight: "1.5" }],
        h3: ["1.5rem", { lineHeight: "1.25" }],
        h2: ["2.125rem", { lineHeight: "1.12" }],
        h1: ["3.25rem", { lineHeight: "1.02" }],
      },
      borderRadius: { plate: "0px", control: "0px" },
      boxShadow: {
        // The only "shadow" in the system: a milled-edge highlight, not a drop shadow.
        plate: "inset 0 1px 0 0 var(--plate-highlight)",
        lift: "inset 0 1px 0 0 var(--plate-highlight), 0 0 0 1px var(--rule-strong)",
      },
      transitionTimingFunction: { snap: "cubic-bezier(0.2, 0.8, 0.2, 1)" },
    },
  },
  plugins: [],
};

export default config;
