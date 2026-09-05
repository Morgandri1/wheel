/**
 * §3c #12: what you type is a client-side draft until you press Send.
 *
 * Nothing typed here reaches the agent — the engine's delivery loop is the only writer to a
 * child's stdin, and a draft becomes a real `messages` row only on Send. Keeping it per agent in
 * localStorage means a reload, a tab switch, or a stray refresh never eats what you were writing.
 */
const PREFIX = "wheel.draft.";

const key = (projectId: string, agentId: string) => `${PREFIX}${projectId}.${agentId}`;

/** localStorage throws in private windows and when site data is blocked; a lost draft is not fatal. */
export function readDraft(projectId: string, agentId: string): string {
  try {
    return window.localStorage.getItem(key(projectId, agentId)) ?? "";
  } catch {
    return "";
  }
}

export function writeDraft(projectId: string, agentId: string, value: string): void {
  try {
    if (value) window.localStorage.setItem(key(projectId, agentId), value);
    else window.localStorage.removeItem(key(projectId, agentId));
  } catch {
    /* the draft simply won't survive a reload */
  }
}

export function clearDraft(projectId: string, agentId: string): void {
  writeDraft(projectId, agentId, "");
}

/** Drop every draft for a project — used when the project is deleted. */
export function clearProjectDrafts(projectId: string): void {
  try {
    const prefix = `${PREFIX}${projectId}.`;
    for (const k of Object.keys(window.localStorage)) {
      if (k.startsWith(prefix)) window.localStorage.removeItem(k);
    }
  } catch {
    /* nothing to clean up */
  }
}
