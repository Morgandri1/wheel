import type { NextConfig } from "next";

const config: NextConfig = {
  // A second dev server — a different auth mode, say — can only run beside the first if it has
  // its own build directory. Two `next dev` sharing one `.next` corrupt each other's cache, and
  // the symptom is a 500 on the server you were not touching.
  distDir: process.env.NEXT_DIST_DIR ?? ".next",
  reactStrictMode: true,
  eslint: { ignoreDuringBuilds: false },
  typescript: { ignoreBuildErrors: false },
};

export default config;
