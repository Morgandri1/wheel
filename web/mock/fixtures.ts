/** A board that already teaches the model: ctx injects into an agent, which relays to a second. */
import type { Project, WheelNode } from "../src/lib/schema";

const PROJECT_ID = "11111111-1111-4111-8111-111111111111";
const CTX_ID = "22222222-2222-4222-8222-222222222222";
const RESEARCHER_ID = "33333333-3333-4333-8333-333333333333";
const WRITER_ID = "44444444-4444-4444-8444-444444444444";

export const seedProject: Project = {
  id: PROJECT_ID,
  owner_id: "user_mock",
  name: "field-notes",
  capabilities: { http: false },
  status: "stopped",
  created_at: "2026-09-01T09:12:00Z",
  updated_at: "2026-09-01T09:12:00Z",
};

export const seedNodes: WheelNode[] = [
  {
    id: CTX_ID,
    name: "house-style",
    type: "ctx",
    position: { x: 80, y: 260 },
    wires: [{ to: RESEARCHER_ID, type: "send" }],
    config: {
      markdown:
        "# House style\n\nWrite in plain sentences. No hedging, no filler.\nCite the source next to the claim, not in a footnote.\n",
    },
    state: null,
  },
  {
    id: RESEARCHER_ID,
    name: "researcher",
    type: "agent",
    position: { x: 420, y: 160 },
    wires: [
      { to: WRITER_ID, type: "send" },
      { to: CTX_ID, type: "read" },
    ],
    config: {
      harness: "claude",
      model: "claude-opus-5",
      system_prompt: "You gather sources and hand the writer a brief with links.",
      run_on_startup: true,
      ephemeral_context: false,
    },
    state: { status: "stopped" },
  },
  {
    id: WRITER_ID,
    name: "writer",
    type: "agent",
    position: { x: 780, y: 340 },
    wires: [],
    config: {
      harness: "codex",
      system_prompt: "You turn the researcher's brief into finished prose.",
      run_on_startup: false,
      ephemeral_context: true,
    },
    state: { status: "stopped" },
  },
];
