"use client";

import { SignIn, SignUp } from "@clerk/nextjs";

/**
 * Clerk's own screens, in a module of their own so `next/dynamic` can keep them out of the bundle
 * a local-mode deploy ships. Clerk is ~50 kB, and the sign-in page is the first thing anyone ever
 * loads — paying for a provider that build is not configured to use is the wrong first impression.
 */
export default function ClerkScreen({ mode }: { mode: "sign-in" | "sign-up" }) {
  return mode === "sign-in" ? <SignIn /> : <SignUp />;
}
