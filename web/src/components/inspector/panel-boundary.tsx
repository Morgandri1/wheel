"use client";

import { Component, type ErrorInfo, type ReactNode } from "react";

/**
 * Keeps one panel's failure to itself.
 *
 * An inspector panel renders whatever the engine sent, and the engine's shapes move faster than
 * this app does. Without a boundary a single bad read — a field the server stopped sending, a
 * null where an object was assumed — unmounts the whole React tree, and what the operator sees is
 * not "the endpoint panel is broken" but a blank page where their board used to be. That is
 * exactly what happened when `Board.project` promised fields the engine never sends.
 *
 * The board, the canvas and every other node stay usable; the failure is scoped to the panel and
 * says what broke, so it can be reported instead of guessed at.
 */
export class PanelBoundary extends Component<
  { children: ReactNode; nodeName: string },
  { error: Error | null }
> {
  state: { error: Error | null } = { error: null };

  static getDerivedStateFromError(error: Error) {
    return { error };
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    console.error(`inspector panel for "${this.props.nodeName}" failed`, error, info.componentStack);
  }

  componentDidUpdate(prev: { nodeName: string }) {
    // Selecting a different node is a fresh attempt; without this the panel stays broken for
    // every node once any node has broken it.
    if (prev.nodeName !== this.props.nodeName && this.state.error) this.setState({ error: null });
  }

  render() {
    if (!this.state.error) return this.props.children;
    return (
      <div
        data-testid="inspector-panel-error"
        className="flex flex-col gap-2 border border-[color-mix(in_srgb,var(--danger)_45%,transparent)] p-2.5"
      >
        <p className="text-micro" style={{ color: "var(--danger)" }}>
          This panel could not be drawn for <span className="ident">{this.props.nodeName}</span>.
          The rest of the board still works.
        </p>
        <p className="text-micro text-ink-faint">{this.state.error.message}</p>
      </div>
    );
  }
}
