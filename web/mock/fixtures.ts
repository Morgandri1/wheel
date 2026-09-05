import type { AgentConfig, CtxConfig, EndpointConfig, TableConfig, VaultConfig } from "@/lib/schema";
import { boardChanged, createProject, makeNode, type ProjectRecord } from "./state";

/**
 * A board that already tells a story: a context node feeding a researcher, the
 * researcher briefing a writer and filing findings, and a public endpoint that
 * wakes the researcher up. Every wire type and the injection wire are present,
 * so the legend has something to explain the moment you open the app.
 */
export function seed(): ProjectRecord {
  const record = createProject("orbit");
  record.project.capabilities.http = true;

  const houseStyle = makeNode("ctx", "house-style", { x: -260, y: 40 }, {
    markdown:
      "# House style\n\n- Lead with the finding, not the method.\n- One claim per paragraph.\n- Cite the source inline; no footnotes.\n- If you are unsure, say so and say why.\n",
  } satisfies CtxConfig);

  const researcher = makeNode("agent", "researcher", { x: 80, y: 0 }, {
    harness: "claude",
    run_on_startup: true,
    ephemeral_context: false,
    model: "claude-opus-4",
    system_prompt:
      "You read what comes in, check it against what is already in `findings`, and hand the writer something worth publishing.",
  } satisfies AgentConfig);

  const writer = makeNode("agent", "writer", { x: 460, y: 130 }, {
    harness: "codex",
    system_prompt: "You turn the researcher's notes into prose that follows the house style.",
    run_on_startup: false,
    ephemeral_context: true,
  } satisfies AgentConfig);

  const findings = makeNode("table", "findings", { x: 460, y: -140 }, {
    columns: [
      { name: "claim", type: "text" },
      { name: "source", type: "text" },
      { name: "confidence", type: "real" },
    ],
  } satisfies TableConfig);

  const inbound = makeNode("endpoint", "inbound", { x: -260, y: -170 }, {
    method: "POST",
    path: "/inbound",
    response_mode: "ack",
  } satisfies EndpointConfig);

  // QA E2E-vault-masked: something real to write a sentinel into. Only the KEY NAMES are ever
  // returned by the board; there is no route, here or in the engine, that reads a value back.
  const credentials = makeNode("vault", "credentials", { x: 80, y: 200 }, {
    keys: ["celestrak_token"],
  } satisfies VaultConfig);

  // QA asked for an agent already parked. §3c #14: parked is a HEALTHY processless state — the
  // session is kept and the next message resumes it — so the UI must not dress it as an error.
  const archivist = makeNode("agent", "archivist", { x: 460, y: 320 }, {
    harness: "claude",
    system_prompt: "You file what the writer finishes, and otherwise cost nothing.",
    run_on_startup: false,
    ephemeral_context: false,
    idle_timeout_secs: 300,
  } satisfies AgentConfig);
  archivist.state = {
    kind: "agent",
    status: "parked",
    session_id: "sess-archivist-resumable",
    last_activity: new Date(Date.now() - 11 * 60_000).toISOString(),
  };

  record.nodes.push(houseStyle, researcher, writer, findings, inbound, credentials, archivist);

  houseStyle.wires!.push({ to: researcher.id, type: "send" }); // injection
  researcher.wires!.push({ to: writer.id, type: "send" });
  researcher.wires!.push({ to: findings.id, type: "write" });
  researcher.wires!.push({ to: houseStyle.id, type: "read" });
  inbound.wires!.push({ to: researcher.id, type: "send" });
  researcher.wires!.push({ to: credentials.id, type: "read" });
  writer.wires!.push({ to: archivist.id, type: "send" });
  // Deliberately NOT wired, and unwirable: `findings` (table) has no outgoing wires at all, so
  // dragging table → agent is the refusal path with no hunting for a combination. Same for
  // `credentials` (vault) → anything, and agent → endpoint in the other direction.

  const rows = new Map<string, Record<string, unknown>>([
    ["orbital-decay", { key: "orbital-decay", claim: "Decay is faster than the 2019 model predicts.", source: "celestrak", confidence: 0.72 }],
    ["launch-cadence", { key: "launch-cadence", claim: "Cadence doubled year over year.", source: "internal", confidence: 0.91 }],
  ]);
  record.tables.set(findings.id, rows);

  boardChanged(record);
  return record;
}
