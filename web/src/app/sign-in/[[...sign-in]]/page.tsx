import { SignIn } from "@clerk/nextjs";

export const metadata = { title: "Sign in — Wheel" };

export default function Page() {
  return (
    <main className="flex min-h-screen items-center justify-center p-6">
      <SignIn />
    </main>
  );
}
