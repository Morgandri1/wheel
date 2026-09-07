import { describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { CollapseButton, SidebarRail } from "./sidebar-rail";

describe("a collapsed sidebar always leaves a way back", () => {
  /**
   * The failure this guards: a panel that collapses to nothing is lost, not closed. The user has
   * no control to click and no reason to think one exists, so the fix is a reload or a guessed
   * shortcut. The rail is what makes collapsing safe to try.
   */
  it("renders a labelled control that reopens the panel", () => {
    const onExpand = vi.fn();
    render(<SidebarRail side="left" label="Palette" onExpand={onExpand} testId="btn-palette-expand" />);
    const rail = screen.getByTestId("btn-palette-expand");
    expect(rail.getAttribute("aria-label")).toBe("Show Palette");
    fireEvent.click(rail);
    expect(onExpand).toHaveBeenCalledOnce();
  });

  it("says which state it is in, so a screen reader is not guessing", () => {
    render(<SidebarRail side="right" label="Inspector" onExpand={() => {}} testId="r" />);
    expect(screen.getByTestId("r").getAttribute("aria-expanded")).toBe("false");
  });
});

describe("CollapseButton", () => {
  it("collapses the panel it sits in", () => {
    const onCollapse = vi.fn();
    render(<CollapseButton side="right" label="Inspector" onCollapse={onCollapse} testId="btn-x" />);
    const btn = screen.getByTestId("btn-x");
    expect(btn.getAttribute("aria-label")).toBe("Hide Inspector");
    expect(btn.getAttribute("aria-expanded")).toBe("true");
    fireEvent.click(btn);
    expect(onCollapse).toHaveBeenCalledOnce();
  });

  it("points the way the panel will go", () => {
    const { rerender } = render(<CollapseButton side="left" label="P" onCollapse={() => {}} testId="b" />);
    expect(screen.getByTestId("b").textContent).toBe("‹");
    rerender(<CollapseButton side="right" label="I" onCollapse={() => {}} testId="b" />);
    expect(screen.getByTestId("b").textContent).toBe("›");
  });
});
