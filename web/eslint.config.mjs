import { dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { FlatCompat } from "@eslint/eslintrc";

/**
 * Flat config, replacing `next lint` — deprecated in Next 15.5 and removed in 16.
 *
 * eslint-config-next 15.5 still ships eslintrc-style configs rather than flat ones, so FlatCompat
 * translates them. When it publishes native flat configs this file collapses to a plain import.
 *
 * `next lint` linted a fixed set of directories; `eslint .` lints everything not ignored, so the
 * ignores below are what keeps generated output (including public/monaco, a copied third-party
 * bundle) from being linted as if we wrote it.
 */
const compat = new FlatCompat({ baseDirectory: dirname(fileURLToPath(import.meta.url)) });

const config = [
  {
    ignores: [
      ".next/**",
      ".next-*/**",
      "coverage/**",
      "node_modules/**",
      "public/monaco/**",
      "next-env.d.ts",
      "src/lib/schema/generated.ts",
    ],
  },
  ...compat.extends("next/core-web-vitals", "next/typescript"),
];

export default config;
