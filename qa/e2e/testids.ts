/**
 * Every data-testid the E2E suite depends on, in ONE place.
 *
 * The suite never selects by visible text. Text is copy: it gets rewritten, translated
 * and A/B-tested, and a suite that asserts on it fails for reasons that have nothing to
 * do with whether the product works — which is how E2E suites earn their reputation and
 * then get ignored.
 *
 * WEB OWNS THESE NAMES. This file previously carried names QA had proposed before Web
 * shipped, and 21 of 27 did not exist — the suite would have failed wholesale, 30s into
 * a browser launch, for reasons unrelated to the product. It is now reconciled against
 * web/src, and `qa/contract/testid_parity.py` fails `make check` the moment it drifts
 * again, in under a second and without launching anything.
 */
export const T = {
  // landing + navigation
  ctaApp: "cta-app",
  linkHome: "link-home",
  linkProjects: "link-projects",

  // projects
  projectList: "project-list",
  projectNew: "btn-new-project",
  projectNewEmpty: "btn-new-project-empty",
  projectNameInput: "input-project-name",
  projectCreateSubmit: "btn-create-project",
  projectStart: "btn-start-project",
  projectLink: (id: string) => `project-link-${id}`,
  projectDeleteConfirmInput: "input-confirm-delete",
  projectDeleteConfirm: "btn-confirm-delete",

  // board canvas
  board: "board-canvas",
  boardProjectName: "board-project-name",
  palette: "palette",
  paletteNode: (type: string) => `palette-${type}`,
  // Keyed by node NAME, not id — that is what web/src/components/board/node-plate.tsx emits.
  node: (name: string) => `node-${name}`,
  nodeName: (name: string) => `node-name-${name}`,
  nodeNameInput: (name: string) => `node-name-input-${name}`,
  nodeStart: (name: string) => `btn-start-${name}`,
  nodeStop: (name: string) => `btn-stop-${name}`,
  nodeDelete: (name: string) => `btn-delete-${name}`,
  wire: (id: string) => `wire-${id}`,
  wireOption: (type: string) => `wire-option-${type}`,
  wireLegend: "wire-legend",
  widePopover: "wire-popover",

  // the UI's channel for a refused action (illegal wire, engine refusal)
  toast: "toast",

  // inspector — Web namespaces per node type rather than a generic field map
  inspectorEmpty: "inspector-empty",
  inspectorAgentHarness: "inspector-agent-harness",
  inspectorAgentModel: "inspector-agent-model",
  inspectorAgentSystemPrompt: "inspector-agent-system-prompt",
  inspectorCtxMarkdown: "inspector-ctx-markdown",
  ctxPreview: "ctx-preview",
  agentSave: "btn-agent-save",
  ctxSave: "btn-ctx-save",

  // agent drawer, logs and chat
  agentDrawer: "agent-drawer",
  drawerToggle: "btn-drawer-toggle",
  drawerTab: (name: string) => `drawer-tab-${name}`,
  agentStart: "btn-agent-start",
  agentStop: "btn-agent-stop",
  agentRestart: "btn-agent-restart",
  agentClear: "btn-agent-clear",
  openLog: "btn-open-log",
  logStream: "log-stream",
  logLine: "log-line",
  logEmpty: "log-empty",
  chatInput: "chat-input",
  chatSend: "chat-send",
  chatInterrupt: "chat-interrupt",
  chatLimitWarning: "chat-limit-warning",
  chatLimitError: "chat-limit-error",
  messageList: "message-list",
  message: (id: string) => `msg-${id}`,

  // auth (per-agent harness login)
  authFlow: "auth-flow",
  authMode: "auth-mode",
  authStatus: "auth-status",
  authNeedsAuthCallout: "auth-needs-auth-callout",
  authChecking: "auth-checking",
  apiKeyInput: "input-api-key",
  authComplete: "btn-auth-complete",
  authReplace: "btn-auth-replace",
  authOauth: "btn-auth-oauth",
  nodeAuthenticate: (name: string) => `node-${name}-authenticate`,

  // DEFERRED(M2): the engine's OAuth flow (§4 auth/begin device_code | paste_code).
  // Web ships M1 as API-key-only and disables btn-auth-oauth with an M2 title; these
  // are the selectors the paste-code/device-code UI will need. testid_parity treats a
  // DEFERRED block as "not yet rendered, and that is expected" — and FAILS when one
  // starts being rendered, which is the signal to write the E2E test.
  // DEFERRED-BEGIN
  authLink: "auth-link",
  authCodeInput: "input-auth-code",
  authUserCode: "auth-user-code",
  // DEFERRED-END

  // local email/password auth (§2 AUTH_MODE=local). Web's names, from their f6a02d2.
  // Only the `local-auth` Playwright project uses these: NEXT_PUBLIC_AUTH_MODE is inlined
  // at build time, so they are rendered by a different server than the default suite's.
  authScreen: "auth-screen",
  authForm: "auth-form",
  emailInput: "input-email",
  passwordInput: "input-password",
  authSubmit: "btn-auth-submit",
  authError: "auth-error",
  authSwitch: "link-auth-switch",
  sessionBadge: "session-badge",
  signOut: "btn-sign-out",
  sessionLoading: "session-loading",
  sessionRedirecting: "session-redirecting",
  authWrongMode: "auth-wrong-mode",

  // connection health
  connIndicator: "conn-indicator",
} as const;
