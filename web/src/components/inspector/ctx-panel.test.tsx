// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import { describe, expect, it } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { CtxPanel } from "./ctx-panel";
import type { CtxNode } from "@/lib/schema";
import type { EngineApi } from "@/lib/api";

const node = {
  id: "c1",
  name: "brief",
  type: "ctx",
  position: { x: 0, y: 0 },
  wires: [],
  config: { markdown: "# Heading\n\nbody text" },
} as unknown as CtxNode;

const api = {} as EngineApi;

describe("the ctx preview renders its markdown", () => {
  /**
   * The preview's markdown stack is the heaviest thing on the board's first load, and it sits
   * behind a tab, so SafeMarkdown is loaded lazily: 259 kB -> 214 kB gzipped, measured by two full
   * builds rather than read off a manifest.
   *
   * A lazy component that never resolves renders an EMPTY container, and every assertion about the
   * container still passes. So this asserts the CONTENT. `React.lazy` rather than `next/dynamic`
   * because next/dynamic does not resolve under jsdom, which would leave the split with no
   * automated proof the preview still works.
   */
  it("renders the markdown, not just the container", async () => {
    render(<CtxPanel node={node} api={api} onChanged={() => {}} />);
    fireEvent.click(screen.getByTestId("ctx-tab-preview"));
    await waitFor(() => expect(screen.queryByText("Heading")).not.toBeNull());
    expect(screen.getByTestId("ctx-preview").innerHTML).toContain("<h1>Heading</h1>");
  });
});
