// Finding 012 — static fills leak in --curl / call url (owner: Web mock + engine).
// renderUrl/renderCurl below are copied VERBATIM from web/mock/server.ts (web/main 85a4028,
// lines 574-608) — they are pure (only erased TS types removed), so this runs the actual logic,
// not a reimplementation. Contract §3d rule 3 (ARCHITECTURE.md:260): "--curl / copy as curl render
// the exact equivalent curl with static/vault values masked" (BOTH). §3d also: static is "never
// shown to the agent". An agent may call `wheel tool call <op> --curl` (dry_run) — see server.ts
// tool call handler, which returns renderCurl after allowing only agent-mode fields.
// Run: node t_curl_static_leak.mjs   (exit 1 = FINDING reproduced)

const renderUrl = (baseUrl, op, args) => {
  let path = op.path;
  const query = [];
  for (const param of op.params ?? []) {
    const mode = param.fill?.mode ?? "agent";
    if (mode === "hidden") continue;
    const value =
      mode === "static" ? (param.fill?.value ?? "")
      : mode === "vault" ? "<from vault>"
      : String(args[param.name] ?? "");
    if (param.location === "path") path = path.replace(`{${param.name}}`, encodeURIComponent(value));
    if (param.location === "query" && value)
      query.push(`${encodeURIComponent(param.name)}=${encodeURIComponent(value)}`);
  }
  return `${baseUrl.replace(/\/$/, "")}${path}${query.length ? `?${query.join("&")}` : ""}`;
};

const renderCurl = (baseUrl, op, args) => {
  const parts = [`curl -X ${op.method}`];
  for (const param of op.params ?? []) {
    if (param.location !== "header") continue;
    const mode = param.fill?.mode ?? "agent";
    if (mode === "hidden") continue;
    const value =
      mode === "vault" ? "****" : mode === "static" ? (param.fill?.value ?? "") : String(args[param.name] ?? "");
    parts.push(`-H '${param.name}: ${value}'`);
  }
  parts.push(`'${renderUrl(baseUrl, op, args)}'`);
  return parts.join(" ");
};

const op = {
  method: "GET", path: "/data/{tenant}",
  params: [
    { name: "X-Vault-Key", location: "header", fill: { mode: "vault", ref: "kv/api-key" } },
    { name: "X-Static-Token", location: "header", fill: { mode: "static", value: "SECRET-STATIC-abc123" } },
    { name: "tenant", location: "path", fill: { mode: "static", value: "acme-secret-tenant" } },
    { name: "region", location: "query", fill: { mode: "static", value: "eu-secret-1" } },
  ],
};

const curl = renderCurl("https://api.example.com", op, {});
console.log("curl:", curl);

const findings = [];
if (curl.includes("SECRET-STATIC-abc123")) findings.push("static HEADER value rendered in cleartext in --curl (contract: must be masked)");
if (curl.includes("acme-secret-tenant")) findings.push("static PATH value rendered in cleartext in the curl URL");
if (curl.includes("eu-secret-1")) findings.push("static QUERY value rendered in cleartext in the curl URL");
if (!curl.includes("****")) findings.push("vault value NOT masked (claim 3 core broken)");

if (findings.length) {
  for (const f of findings) console.log("FAIL: FINDING —", f);
  process.exit(1);
}
console.log("PASS: resisted — all static and vault values masked");
