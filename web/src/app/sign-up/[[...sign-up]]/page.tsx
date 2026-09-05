import { SignUp } from "@clerk/nextjs";

export const metadata = { title: "Create an account — Wheel" };

export default function Page() {
  return (
    <main className="flex min-h-screen items-center justify-center p-6">
      <SignUp />
    </main>
  );
}
