import { AuthRoute } from "@/components/auth/auth-route";

export const metadata = { title: "Create account — Wheel" };

export default function Page() {
  return (
    <main className="flex min-h-screen items-center justify-center p-6">
      <AuthRoute mode="sign-up" />
    </main>
  );
}
