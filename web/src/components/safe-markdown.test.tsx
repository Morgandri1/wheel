import { describe, expect, it } from "vitest";
import { render } from "@testing-library/react";
import { SafeMarkdown } from "./safe-markdown";

/**
 * Real payloads, asserted against the rendered DOM.
 *
 * A ctx node's markdown is written by agents, and an agent is untrusted code by contract. If any
 * of these ever renders as live markup, the session token in localStorage is readable by whoever
 * wrote the ctx node — so these assertions are the boundary, not a formality.
 */
const PAYLOADS: [name: string, markdown: string][] = [
  ["a script tag", `<script>window.__pwned = 1</script>`],
  ["an image error handler", `<img src=x onerror="window.__pwned = 1">`],
  ["an svg load handler", `<svg onload="window.__pwned = 1"></svg>`],
  ["an iframe", `<iframe src="https://evil.example"></iframe>`],
  ["an inline event on a div", `<div onmouseover="window.__pwned = 1">hover</div>`],
  ["an object tag", `<object data="https://evil.example"></object>`],
  ["a form posting elsewhere", `<form action="https://evil.example"><input name="t"></form>`],
  ["a style tag", `<style>body { display: none }</style>`],
  ["a base tag", `<base href="https://evil.example/">`],
  ["a meta refresh", `<meta http-equiv="refresh" content="0;url=https://evil.example">`],
];

describe("markdown from an untrusted agent", () => {
  it.each(PAYLOADS)("renders %s inert", (_name, markdown) => {
    const { container } = render(<SafeMarkdown>{markdown}</SafeMarkdown>);
    const html = container.innerHTML;

    expect(container.querySelector("script")).toBeNull();
    expect(container.querySelector("iframe")).toBeNull();
    expect(container.querySelector("object")).toBeNull();
    expect(container.querySelector("form")).toBeNull();
    expect(container.querySelector("style")).toBeNull();
    expect(container.querySelector("base")).toBeNull();
    expect(container.querySelector("meta")).toBeNull();
    expect(html).not.toMatch(/onerror|onload|onmouseover/i);
    expect((window as unknown as { __pwned?: number }).__pwned).toBeUndefined();
  });

  it.each([
    ["javascript:", `[click](javascript:window.__pwned=1)`],
    ["a data: url", `[click](data:text/html;base64,PHNjcmlwdD5hbGVydCgxKTwvc2NyaXB0Pg==)`],
    ["vbscript:", `[click](vbscript:msgbox(1))`],
    ["a case-shifted scheme", `[click](JaVaScRiPt:window.__pwned=1)`],
  ])("strips %s from a link", (_name, markdown) => {
    const { container } = render(<SafeMarkdown>{markdown}</SafeMarkdown>);
    const href = container.querySelector("a")?.getAttribute("href") ?? "";
    expect(href).not.toMatch(/^\s*(javascript|data|vbscript):/i);
  });

  it.each([
    ["a javascript: image source", `![x](javascript:window.__pwned=1)`],
    ["a data: image source", `![x](data:image/svg+xml;base64,PHN2ZyBvbmxvYWQ9ImFsZXJ0KDEpIi8+)`],
  ])("strips %s", (_name, markdown) => {
    const { container } = render(<SafeMarkdown>{markdown}</SafeMarkdown>);
    const src = container.querySelector("img")?.getAttribute("src") ?? "";
    expect(src).not.toMatch(/^\s*(javascript|data):/i);
  });

  it("still renders the markdown people actually write", () => {
    const { container } = render(
      <SafeMarkdown>{"# Title\n\nSome **bold** text and a [link](https://wheel.dev).\n\n- one\n- two"}</SafeMarkdown>,
    );
    expect(container.querySelector("h1")?.textContent).toBe("Title");
    expect(container.querySelector("strong")?.textContent).toBe("bold");
    expect(container.querySelector("a")?.getAttribute("href")).toBe("https://wheel.dev");
    expect(container.querySelectorAll("li")).toHaveLength(2);
  });
});
