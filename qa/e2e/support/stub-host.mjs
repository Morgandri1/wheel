// A stand-in for wheel-host, just enough for the real wheel-api to create and serve projects.
// It records every engine request it is sent, so a test can prove a refused call never got here.
import http from "node:http";

const port = Number(process.env.STUB_HOST_PORT ?? 8791);
const secret = process.env.STUB_HOST_SECRET ?? "stub-host-secret-0123456789";
const engineHits = [];

const json = (res, status, body) => {
  res.writeHead(status, { "content-type": "application/json" });
  res.end(JSON.stringify(body));
};

http
  .createServer((req, res) => {
    const url = new URL(req.url, "http://stub");
    if (url.pathname === "/healthz") return json(res, 200, { ok: true });
    if (url.pathname === "/__engine-hits") {
      if (req.method === "DELETE") engineHits.length = 0;
      return json(res, 200, engineHits);
    }
    if (req.headers.authorization !== `Bearer ${secret}`) return json(res, 401, { error: "bearer" });
    const m = /^\/host\/v1\/projects\/([^/]+)(\/.*)?$/.exec(url.pathname);
    if (!m) return json(res, 404, {});
    const [, id, rest = ""] = m;
    if (rest.startsWith("/engine/")) {
      const path = rest.slice("/engine".length);
      engineHits.push({ method: req.method, path, project: id });
      if (path === "/v1/board") {
        return json(res, 200, { nodes: [], project: { id, name: "stub" } });
      }
      return json(res, 200, {});
    }
    if (req.method === "GET" && rest === "") return json(res, 200, { status: "running" });
    return json(res, 200, {});
  })
  .listen(port, "127.0.0.1", () => console.log(`stub host on ${port}`));
