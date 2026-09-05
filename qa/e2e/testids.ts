/**
 * Every data-testid the E2E suite depends on, in ONE place.
 *
 * The suite never selects by visible text. Text is copy: it gets rewritten, translated
 * and A/B-tested, and a suite that asserts on it fails for reasons that have nothing to
 * do with whether the product works — which is how E2E suites earn their reputation and
 * then get ignored.
 *
 * QA proposes these names; Web owns them. If Web already has a convention, this file
 * changes and nothing else does.
 */
export const T = {
  // landing + auth
  landingHero: "landing-hero",
  signInButton: "sign-in",
  appRoot: "app-root",

  // projects
  projectNew: "project-new",
  projectNameInput: "project-name-input",
  projectCreateSubmit: "project-create-submit",
  projectCard: (id: string) => `project-card-${id}`,
  projectStatus: "project-status",

  // board canvas
  board: "board",
  paletteNode: (type: string) => `palette-${type}`,
  node: (name: string) => `node-${name}`,
  nodeStatus: (name: string) => `node-status-${name}`,
  wire: (from: string, to: string, type: string) => `wire-${from}-${to}-${type}`,
  wireError: "wire-error",

  // inspector
  inspector: "inspector",
  inspectorField: (key: string) => `inspector-field-${key}`,
  inspectorSave: "inspector-save",

  // agent drawer
  agentStart: "agent-start",
  agentStop: "agent-stop",
  agentLog: "agent-log",
  agentLogLine: "agent-log-line",
  chatInput: "chat-input",
  chatSend: "chat-send",
  messageRow: (id: string) => `message-${id}`,
  messageState: (id: string) => `message-state-${id}`,

  // vault
  vaultKeyRow: (key: string) => `vault-key-${key}`,
  vaultValueInput: "vault-value-input",
} as const;
