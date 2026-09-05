"use client";

import { create } from "zustand";
import type { ConnectionStatus } from "@/lib/events";
import type { EngineFrame, LogLine, Message, NodeType, WireDenial, WireType } from "@/lib/schema";

/** Per-agent log ring. Old lines fall off so a chatty agent can't grow the tab without bound. */
const LOG_CAP = 2000;

export interface PendingWire {
  from: string;
  to: string;
  fromType: NodeType;
  toType: NodeType;
  /** Screen position for the type popover. */
  at: { x: number; y: number };
}

interface BoardState {
  selectedNodeId: string | null;
  select: (id: string | null) => void;

  /** Agent ids with an open tab in the bottom drawer, in the order they were opened. */
  drawerTabs: string[];
  activeTab: string | null;
  openTab: (id: string) => void;
  closeTab: (id: string) => void;
  setActiveTab: (id: string) => void;
  drawerOpen: boolean;
  setDrawerOpen: (open: boolean) => void;

  pendingWire: PendingWire | null;
  setPendingWire: (w: PendingWire | null) => void;

  connection: ConnectionStatus;
  setConnection: (c: ConnectionStatus) => void;

  logs: Record<string, LogLine[]>;
  messages: Message[];
  /** Recent wire denials, newest last — an agent reaching for something it was never wired to. */
  denials: WireDenial[];
  /** Node ids whose runtime state changed in the last batch — consumers re-read the board query. */
  applyEvents: (events: EngineFrame[]) => { stateChanged: boolean; boardChanged: boolean; lagged: boolean };
  seedLog: (nodeId: string, lines: LogLine[]) => void;
  reset: () => void;
}

export const useBoardStore = create<BoardState>((set, get) => ({
  selectedNodeId: null,
  select: (id) => set({ selectedNodeId: id }),

  drawerTabs: [],
  activeTab: null,
  drawerOpen: false,
  openTab: (id) =>
    set((s) => ({
      drawerTabs: s.drawerTabs.includes(id) ? s.drawerTabs : [...s.drawerTabs, id],
      activeTab: id,
      drawerOpen: true,
    })),
  closeTab: (id) =>
    set((s) => {
      const tabs = s.drawerTabs.filter((t) => t !== id);
      return {
        drawerTabs: tabs,
        activeTab: s.activeTab === id ? (tabs[tabs.length - 1] ?? null) : s.activeTab,
        drawerOpen: tabs.length > 0 && s.drawerOpen,
      };
    }),
  setActiveTab: (id) => set({ activeTab: id, drawerOpen: true }),
  setDrawerOpen: (drawerOpen) => set({ drawerOpen }),

  pendingWire: null,
  setPendingWire: (pendingWire) => set({ pendingWire }),

  connection: "connecting",
  setConnection: (connection) => set({ connection }),

  logs: {},
  messages: [],
  denials: [],

  seedLog: (nodeId, lines) =>
    set((s) => ({ logs: { ...s.logs, [nodeId]: lines.slice(-LOG_CAP) } })),

  applyEvents: (events) => {
    let stateChanged = false;
    let boardChanged = false;
    let lagged = false;
    const logs = { ...get().logs };
    let messages = get().messages;
    let denials = get().denials;
    let touchedLogs = false;

    for (const e of events) {
      switch (e.type) {
        case "log": {
          const prev = logs[e.line.node_id] ?? [];
          const next = prev.concat(e.line);
          logs[e.line.node_id] = next.length > LOG_CAP ? next.slice(next.length - LOG_CAP) : next;
          touchedLogs = true;
          break;
        }
        case "message":
          messages = messages.concat(e.message).slice(-500);
          break;
        case "node.state":
          stateChanged = true;
          break;
        case "board.changed":
          boardChanged = true;
          break;
        // The socket dropped frames rather than let a slow reader stall the engine's delivery
        // loop. The connection is fine; what we hold is stale. Refetch, do not tear down.
        case "lagged":
          lagged = true;
          boardChanged = true;
          break;
        case "wire.denied":
          // An agent tried to reach a node it has no wire to. Surfacing it is §4's whole point:
          // the person sees the capability that is missing, not a silent no-op in a log.
          denials = denials.concat(e.denial).slice(-50);
          break;
      }
    }

    if (touchedLogs || messages !== get().messages || denials !== get().denials) {
      set({ logs, messages, denials });
    }
    return { stateChanged, boardChanged, lagged };
  },

  reset: () =>
    set({
      selectedNodeId: null,
      drawerTabs: [],
      activeTab: null,
      drawerOpen: false,
      pendingWire: null,
      logs: {},
      messages: [],
      denials: [],
      connection: "connecting",
    }),
}));

export type { WireType };
