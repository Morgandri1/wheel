import type { Metadata } from "next";
import { Archivo, JetBrains_Mono } from "next/font/google";
import { Providers } from "@/app/providers";
import { RuntimeConfig } from "@/components/runtime-config";
import { serverApiBaseUrl } from "@/lib/runtime-config";
import "./globals.css";

/**
 * Every route renders per request.
 *
 * The CSP nonce is minted per request by middleware, and a statically prerendered page was built
 * before any request existed — its HTML carries no nonce, so the browser refuses Next's own
 * bootstrap scripts and the page is blank. Verified rather than assumed: with prerendering on,
 * the landing page served 0 nonces and 12 scripts were refused, while a dynamic route served 1
 * and loaded cleanly.
 *
 * The cost is that the landing HTML is no longer CDN-cacheable; static assets still are. That is
 * the price of a nonce-based policy with no 'unsafe-inline', which ADVERSARY R7 makes binding.
 */
export const dynamic = "force-dynamic";

const archivo = Archivo({
  subsets: ["latin"],
  axes: ["wdth"],
  variable: "--font-archivo",
  display: "swap",
});

const mono = JetBrains_Mono({
  subsets: ["latin"],
  variable: "--font-mono",
  display: "swap",
});

export const metadata: Metadata = {
  title: "Wheel",
  description:
    "A board of agents, wired to what they're allowed to touch, running in a container that never sleeps.",
};

export default function RootLayout({ children }: { children: React.ReactNode }) {
  return (
    <html lang="en" data-theme="dark" suppressHydrationWarning>
      <body className={`${archivo.variable} ${mono.variable}`}>
        {/* Before Providers, so the URL is recorded ahead of the first query. */}
        <RuntimeConfig apiUrl={serverApiBaseUrl()} />
        <Providers>{children}</Providers>
      </body>
    </html>
  );
}
