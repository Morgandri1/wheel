/**
 * ENG-events-replay / ENG-log-stream-parity — the WebSocket stream must carry the SAME
 * log streams the database does.
 *
 * Adapted from SDK's ws-live2.mjs probe, with one deliberate change: theirs PRINTS the
 * two sides for a human to compare, this one ASSERTS and exits non-zero. SDK's own e2e
 * passed the transcript bug because it asserted that *a* log event arrived rather than
 * WHICH streams did — a printer cannot fail, so it cannot gate.
 *
 * Needs a live stack (infra/docker-compose.yml). Exits 77 when one isn't reachable, so
 * `make check` reports it as unavailable rather than as a pass.
 *
 *   node qa/live/ws_streams_parity.mjs
 */
import { createHmac } from "node:crypto";

const API = process.env.WHEEL_API_URL ?? "http://localhost:8080";
const SECRET = process.env.AUTH_DEV_SECRET ?? "dev-only-hs256-secret";
const ISSUER = process.env.CLERK_ISSUER ?? "https://dev.wheel.local";
const SKIP = 77;

const b64u = (b) => Buffer.from(b).toString("base64url");
function mint(sub = "user_qa_wsparity") {
  const now = Math.floor(Date.now() / 1000);
  const h = b64u(JSON.stringify({ alg: "HS256", typ: "JWT" }));
  const p = b64u(JSON.stringify({ sub, iss: ISSUER, exp: now + 3600, nbf: now - 60 }));
  return `${h}.${p}.` + createHmac("sha256", SECRET).update(`${h}.${p}`).digest("base64url");
}
const TOKEN = mint();

async function call(method, path, body, pid) {
  const r = await fetch(API + path, {
    method,
    headers: {
      "x-auth-token": TOKEN,
      ...(pid ? { "x-project-id": pid } : {}),
      ...(body ? { "content-type": "application/json" } : {}),
    },
    body: body ? JSON.stringify(body) : undefined,
  });
  const t = await r.text();
  let parsed = null;
  try { parsed = t ? JSON.parse(t) : null; } catch { parsed = t; }
  return [r.status, parsed];
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const fails = [];
const check = (name, cond, detail = "") => {
  if (cond) console.log(`  ok   ${name}`);
  else { console.log(`  FAIL ${name}${detail ? ` — ${detail}` : ""}`); fails.push(name); }
};

let pid;
try {
  const probe = await fetch(API + "/healthz").catch(() => null);
  if (!probe || !probe.ok) {
    console.log(`no API at ${API} — bring up infra/docker-compose.yml (or set WHEEL_API_URL)`);
    process.exit(SKIP);
  }

  const [, pr] = await call("POST", "/v1/projects", { name: `qa-wsparity-${Date.now().toString(36)}` });
  pid = pr?.id;
  if (!pid) { console.log("could not create a project"); process.exit(SKIP); }

  await call("POST", `/v1/projects/${pid}/start`, undefined, pid);
  let running = false;
  for (let i = 0; i < 40; i++) {
    const [, x] = await call("GET", `/v1/projects/${pid}`, undefined, pid);
    if (x?.status === "running") { running = true; break; }
    if (x?.status === "error") break;
    await sleep(1000);
  }
  if (!running) {
    console.log("project never reached running — engine image probably not built yet");
    process.exit(SKIP);
  }

  const E = `/v1/projects/${pid}/engine/v1`;
  const [, agent] = await call("POST", `${E}/nodes`, {
    name: "planner", type: "agent", position: { x: 0, y: 0 },
    config: { harness: "claude", system_prompt: "", run_on_startup: false, ephemeral_context: false },
  }, pid);

  const [, ticket] = await call("POST", `/v1/projects/${pid}/ws-ticket`, undefined, pid);
  const wsUrl = API.replace(/^http/, "ws") + `${E}/events?ticket=${encodeURIComponent(ticket.ticket)}`;
  const frames = [];
  const ws = new WebSocket(wsUrl);
  await new Promise((res, rej) => {
    ws.addEventListener("open", res, { once: true });
    ws.addEventListener("error", () => rej(new Error("ws error")), { once: true });
    setTimeout(() => rej(new Error("ws open timeout")), 10_000);
  });
  ws.addEventListener("message", (e) => {
    try { frames.push(JSON.parse(String(e.data))); } catch { /* non-JSON frame */ }
  });

  await call("POST", `${E}/agents/${agent.id}/auth/complete`,
             { api_key: "sk-test-not-a-real-key" }, pid);
  await call("POST", `${E}/agents/${agent.id}/start`, undefined, pid);
  await sleep(4000);
  await call("POST", `${E}/agents/${agent.id}/send`, { body: "transcript probe" }, pid);
  await sleep(8000);

  const [, all] = await call("GET", `${E}/agents/${agent.id}/log`, undefined, pid);
  const dbLines = all?.lines ?? [];
  const dbStreams = new Set(dbLines.map((l) => l.stream));
  const wsLog = frames.filter((f) => f.type === "log" && f.line);
  const wsStreams = new Set(wsLog.map((f) => f.line.stream));

  console.log(`  WS frame types : ${[...new Set(frames.map((f) => f.type))].join(", ") || "(none)"}`);
  console.log(`  WS log streams : ${[...wsStreams].join(", ") || "(none)"}`);
  console.log(`  DB log streams : ${[...dbStreams].join(", ") || "(none)"}`);

  check("the WS delivered log frames at all", wsLog.length > 0);
  check("the DB recorded log lines at all", dbLines.length > 0);

  // The actual contract: neither side may carry a stream the other doesn't.
  const missingFromWs = [...dbStreams].filter((s) => !wsStreams.has(s));
  const missingFromDb = [...wsStreams].filter((s) => !dbStreams.has(s));
  check("every DB stream also arrived over the WS", missingFromWs.length === 0,
        `missing over WS: ${missingFromWs.join(", ")}`);
  check("every WS stream is also persisted", missingFromDb.length === 0,
        `missing in DB: ${missingFromDb.join(", ")}`);

  // An unknown ?stream= must not silently fall back to "everything".
  const [stBogus, bBogus] = await call("GET", `${E}/agents/${agent.id}/log?stream=bogus`, undefined, pid);
  const bogusLines = bBogus?.lines?.length ?? 0;
  check("?stream=bogus does not return all lines",
        stBogus >= 400 || bogusLines === 0,
        `got ${stBogus} with ${bogusLines} lines (total ${dbLines.length})`);

  ws.close();
} catch (e) {
  console.log(`probe error: ${e?.message ?? e}`);
  fails.push("probe threw");
} finally {
  if (pid) await call("DELETE", `/v1/projects/${pid}`, undefined, pid).catch(() => {});
}

console.log("");
if (fails.length) {
  console.log(`ws stream parity: ${fails.length} FAILED`);
  process.exit(1);
}
console.log("ws stream parity: WS and DB agree on log streams");
