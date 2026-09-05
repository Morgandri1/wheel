"use client";

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { useState } from "react";
import { AUTH_MODE } from "@/lib/auth";
import { ClerkTokenBridge } from "@/components/clerk-bridge";
import { ToastHost } from "@/components/ui/toast";

export function Providers({ children }: { children: React.ReactNode }) {
  const [client] = useState(
    () =>
      new QueryClient({
        defaultOptions: {
          queries: {
            staleTime: 5_000,
            retry: (count, error) => {
              const status = (error as { status?: number })?.status ?? 0;
              if (status === 401 || status === 403 || status === 404) return false;
              return count < 2;
            },
            refetchOnWindowFocus: false,
          },
        },
      }),
  );

  return (
    <QueryClientProvider client={client}>
      {AUTH_MODE === "clerk" ? <ClerkTokenBridge /> : null}
      {children}
      <ToastHost />
    </QueryClientProvider>
  );
}
