// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import { AuthRoute } from "@/components/auth/auth-route";

export const metadata = { title: "Create account — Wheel" };

export default function Page() {
  return (
    <main className="flex min-h-screen items-center justify-center p-6">
      <AuthRoute mode="sign-up" />
    </main>
  );
}
