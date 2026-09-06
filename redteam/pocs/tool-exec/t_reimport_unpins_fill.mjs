// Finding: re-import silently UN-PINS a vault/static fill via a param RENAME. Owner: SDK/Engine.
// merge_operations ported VERBATIM from crates/wheel-engine/src/api/tool_routes.rs:114-149 @ HEAD
// (only Rust->JS syntax changed). Its own doc says (l.106-109): "re-importing a spec must not hand a
// field back to the agent that an operator had pinned to a vault or a fixed value." This runs the real
// logic to show that a RENAMED param defeats that, unreported.
// Run: node t_reimport_unpins_fill.mjs   (exit 1 = finding reproduced)

// verbatim: match old by method+path; keep id/enabled; copy fill ONLY when param NAME matches.
function merge_operations(existing, fresh) {
  const merged = [], added = [];
  for (const op of fresh.map((o) => structuredClone(o))) {
    const old = existing.find((o) => o.method === op.method && o.path === op.path);
    if (old) {
      op.id = old.id;
      op.enabled = old.enabled;
      for (const p of op.params) {
        const prev = old.params.find((q) => q.name === p.name);
        if (prev) p.fill = structuredClone(prev.fill); // else: p keeps its FRESH default fill (agent)
      }
    } else {
      added.push(op.id);
    }
    merged.push(op);
  }
  const removed = existing
    .filter((old) => !merged.some((n) => n.method === old.method && n.path === old.path))
    .map((o) => o.id);
  return { merged, added, removed };
}

const agent = { mode: "agent" };
const vault = { mode: "vault", vault_ref: "prod-keys/API_KEY" };

// Operator-configured tool: op "getData" GET /data, header "Authorization" PINNED to a vault secret.
const existing = [{
  id: "getData", method: "GET", path: "/data", enabled: true,
  params: [{ name: "Authorization", location: "header", fill: vault, required: false }],
}];

// Upstream spec revision renames the header param (Authorization -> authorization) — same endpoint,
// same method+path. Fresh params default to agent-mode on import.
const fresh = [{
  id: "getData", method: "GET", path: "/data", enabled: true,
  params: [{ name: "authorization", location: "header", fill: agent, required: false }],
}];

const { merged, added, removed } = merge_operations(existing, fresh);
const p = merged[0].params.find((x) => x.name === "authorization");
console.log("merged param:", JSON.stringify(p));
console.log("added:", added, " removed:", removed, "  <- op NOT reported changed (method+path unchanged)");

const findings = [];
// The vault pin is gone and the field is now agent-fillable:
if (p.fill.mode === "agent") {
  findings.push("param RENAME (Authorization -> authorization) drops the operator's VAULT pin: the credential header is now AGENT-fillable, i.e. the agent can set the API key it was never meant to see. And added/removed are empty, so the operator gets NO signal.");
}
// sanity: the old vault-pinned name no longer exists in the merged op
if (!merged[0].params.some((x) => x.name === "Authorization")) {
  console.log("(the old 'Authorization' vault param is gone entirely)");
}

if (findings.length) {
  console.log("");
  for (const f of findings) console.log("FAIL:", f);
  process.exit(1);
}
console.log("PASS: fill preserved");
