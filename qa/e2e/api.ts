/**
 * Board setup through the API, not through the UI.
 *
 * Web's advice, and it is right: driving twenty clicks before the assertion starts is how
 * E2E suites become slow and flaky, and a failure in the setup clicks then reads as a
 * failure of whatever the test was actually about. So state is built over HTTP and the
 * browser is used only for what only a browser can check.
 */
const API = process.env.NEXT_PUBLIC_API_URL ?? "http://localhost:8787";
const TOKEN = process.env.WHEEL_E2E_TOKEN ?? "dev";

async function call<T>(method: string, path: string, body?: unknown): Promise<T> {
  const res = await fetch(API + path, {
    method,
    headers: { "x-auth-token": TOKEN, "content-type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  if (!res.ok) throw new Error(`${method} ${path} -> ${res.status} ${await res.text()}`);
  const text = await res.text();
  return (text ? JSON.parse(text) : undefined) as T;
}

export type Node = {
  id: string;
  name: string;
  type: string;
  config: Record<string, unknown>;
  wires?: { to: string; type: string }[];
};

export async function createProject(name: string) {
  return call<{ id: string; name: string }>("POST", "/v1/projects", { name });
}

export async function deleteProject(id: string) {
  await call("DELETE", `/v1/projects/${id}`).catch(() => undefined);
}

export async function addNode(project: string, node: Partial<Node> & { name: string; type: string }) {
  return call<Node>("POST", `/v1/projects/${project}/engine/v1/nodes`, {
    position: { x: 0, y: 0 },
    config: {},
    ...node,
  });
}

export async function addWire(project: string, from: string, to: string, type: string) {
  return call<unknown>("POST", `/v1/projects/${project}/engine/v1/wires`, { from, to, type });
}

/** Attempt a wire and report the refusal instead of throwing — for the deny-path tests. */
export async function tryWire(project: string, from: string, to: string, type: string) {
  const res = await fetch(`${API}/v1/projects/${project}/engine/v1/wires`, {
    method: "POST",
    headers: { "x-auth-token": TOKEN, "content-type": "application/json" },
    body: JSON.stringify({ from, to, type }),
  });
  return { status: res.status, body: await res.text() };
}

export async function board(project: string) {
  return call<{ nodes: Node[] }>("GET", `/v1/projects/${project}/engine/v1/board`);
}

export async function putSecret(project: string, vaultId: string, key: string, value: string) {
  const res = await fetch(`${API}/v1/projects/${project}/engine/v1/vault/${vaultId}/${key}`, {
    method: "PUT",
    headers: { "x-auth-token": TOKEN, "content-type": "application/json" },
    body: JSON.stringify({ value }),
  });
  return res.status;
}
