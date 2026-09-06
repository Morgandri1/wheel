import type { NextConfig } from "next";

const config: NextConfig = {
  // A second dev server — a different auth mode, say — can only run beside the first if it has
  // its own build directory. Two `next dev` sharing one `.next` corrupt each other's cache, and
  // the symptom is a 500 on the server you were not touching.
  distDir: process.env.NEXT_DIST_DIR ?? ".next",
  /**
   * `npx wheel-web` ships a prebuilt server, which needs the standalone output — a self-contained
   * server.js plus only the node_modules it actually uses.
   *
   * Behind a flag rather than always on, because Vercel does its own tracing and output packing;
   * turning this on there changes what gets deployed for no benefit. WHEEL_STANDALONE=1 is set by
   * the packaging script and by nothing else.
   */
  ...(process.env.WHEEL_STANDALONE === "1" ? { output: "standalone" as const } : {}),
  reactStrictMode: true,
  eslint: { ignoreDuringBuilds: false },
  typescript: { ignoreBuildErrors: false },
};

export default config;
