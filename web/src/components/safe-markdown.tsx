"use client";

import ReactMarkdown from "react-markdown";
import rehypeSanitize, { defaultSchema } from "rehype-sanitize";
import remarkGfm from "remark-gfm";

/**
 * The one place markdown becomes DOM.
 *
 * Everything rendered here is attacker-influenced: a ctx node's markdown can be written by any
 * agent with a `write` wire, and an agent is untrusted remote code execution by contract (§2).
 * A single XSS on this board reads the session token out of localStorage, so this is the file
 * that keeps the auth tradeoff in web/DEPLOY.md bounded (ADVERSARY R7, binding).
 *
 * react-markdown does not pass raw HTML through without `rehype-raw`, so the sanitiser below is
 * belt to that braces — deliberately, because "we are safe as long as nobody adds one plugin"
 * is not a property anyone can see while adding the plugin. The schema is the upstream default
 * with the URL protocols narrowed: no `javascript:`, no `data:` in an href.
 */
const SCHEMA = {
  ...defaultSchema,
  protocols: {
    ...defaultSchema.protocols,
    href: ["http", "https", "mailto"],
    src: ["http", "https"],
  },
};

export function SafeMarkdown({ children }: { children: string }) {
  return (
    <ReactMarkdown remarkPlugins={[remarkGfm]} rehypePlugins={[[rehypeSanitize, SCHEMA]]}>
      {children}
    </ReactMarkdown>
  );
}
