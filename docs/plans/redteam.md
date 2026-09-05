# Red Team Plan — ADVERSARY (worktree: redteam/main)

Owner: ADVERSARY. Scope: `redteam/` only. Read-only everywhere else. Attack the LOCAL dev
stack only (`infra/docker-compose.yml`). Metadata/SSRF targets are mocked, never real.

## Guiding stance
Every layer is hostile to every other layer. Wheel runs untrusted, LLM-driven code (agents,
scripts, MCP servers, inbound HTTP) inside per-user sandboxes, orchestrated by an API/host with
sandbox-lifecycle + secret custody, multi-tenant on one kernel in prod. Success = confirmed,
reproducible findings with PoCs QA can regress — not volume.

## Milestones
- **M0 (now):** THREAT-MODEL.md; docs/plans/redteam.md; design-level findings on contract v1.1
  (review, before code exists). Deliver findings as `redteam/findings/NNN-slug.md`.
- **M1:** Stand up local stack as soon as API+SDK are bootable. Campaign 1 (API tenancy) +
  Campaign 2 (proxy/ingress path). Then Campaign 3 (engine token/wire). Automated PoCs → redteam/pocs/.
- **M2:** sqlite/chest/script/MCP escapes; tool-node SSRF (mocked metadata + docker net);
  prompt-injection / envelope-forgery framing; ingress smuggling.
- **M3:** Verify all reported fixes (re-run PoCs, update status); process-backend uid-isolation
  review (highest priority once it exists); container hardening review; REPORT.md.

## Campaign backlog (each → findings/NNN-slug.md + pocs/ where automatable)
1. **API tenancy** (owner API): cross-tenant x-project-id; UUID case/whitespace; A-token+B-project
   over proxy/ingress/WS; JWT alg=none, RS256↔HS256 key confusion, wrong iss/aud, exp/nbf, unknown
   kid JWKS-flood; dev secret in prod; header-alias precedence (Authorization vs x-auth-token).
2. **Proxy/ingress** (owner API): `/engine/../`, `%2e%2e`, double-encode to reach `/v1/cli/*` or
   `/ingress` via authed proxy, or `/v1/*` via public ingress; header smuggling (x-wheel-*, forwarded
   Authorization/engine bearer); WS upgrade on non-WS routes; slowloris/body-size; SSRF via mcp.url /
   script to docker net (wheel-p-<other>:7000, api, host, 169.254.169.254 MOCKED).
3. **Engine wire enforcement** (owner SDK): forge/guess node tokens; token reuse after
   delete/rename/rewire; wire-delete vs CLI-call race; write-vs-read on table/chest; SQL escapes
   (ATTACH, PRAGMA, sqlite_master, t_other, CTE/UNION, load_extension, readfile/writefile); chest
   traversal (.., abs, symlink, unicode-norm, NUL); name-collision / t_-prefix shadowing.
4. **Child-process isolation** (owner SDK): engine secret / vault key / sibling tokens via /proc,
   env, /data, sqlite perms; agent reading /data/wheel.db directly; agent hitting :7000 with node
   token; script timeout/output-cap escape; fork bomb; MCP command = arbitrary binary as engine uid;
   agent editing own/sibling harness creds dir.
5. **Tool nodes §3d** (owner SDK): SSRF via base_url/redirect/DNS-rebind/IPv6-octal-decimal/
   *.railway.internal; CRLF header injection via agent fields; agent overriding static/vault fills
   (dup keys, case, JSON-pointer); vault echo in body/logs/curl; malicious spec DoS (YAML bomb,
   $ref loop, huge body).
6. **Prompt injection / envelope** (owner SDK): forge `<AgentPrompt>` framing to impersonate
   user/another node; unescaped `</AgentPrompt>` in body; attribution forgery; ctx-injected markdown
   that exfiltrates vault via wheel msg/HTTP; confirm vault never in reachable logs/board state.
7. **Stdin race §3c#12** (owner SDK): is delivery loop the SOLE stdin writer? can /send, MCP,
   scripts, log/exec paths write stdin? mid-turn injection; priority-lane jump by a queued agent msg;
   M2 interrupt cancel-then-deliver race.
8. **Multi-tenant host — process backend** (owner API/SDK, TOP once it exists): per-project uid
   enforced for every child; /data/projects/<other> + /run/wheel/<other>/engine.sock cross-uid
   unreachable; /proc/<pid>/environ leak; agents reaching *.railway.internal / host :7100 / host
   secret; rlimit/fork-bomb starvation; setuid-drop correctness (supp groups, no_new_privs).
9. **Container/host — docker backend** (owner API/SDK): docker-socket exposure from host process;
   cap-drop/no-new-privileges/pids-limit actually applied (inspect); volume-name collision; log
   injection (ANSI/newline) into UI; env secrets in docker inspect (by-design residual → propose
   secrets-file).
10. **Web** (owner Web): XSS via node name/markdown/log line; Clerk token in URL/localStorage; CSRF
    / CORS config; clickjacking on OAuth device-code UI.

## Reporting
- To PM: `BUG: <title> | <severity> | redteam/findings/<file> | owner: <API|SDK/Engine|Web>`.
- Always `yoke msg PM --file <f>`. Never fix product code — attach a proposed-patch diff.
- Verification loop: owner says fixed → re-run PoC → update finding Status → STATUS to PM.

## Open questions / risks
- Local stack bootability gates campaigns 1-9; design-review findings (M0) do not wait.
- Metadata SSRF must be mocked (RoE) — I will stand up a fake 169.254.169.254 responder in pocs/.
- Process backend may not exist until M3; its isolation is the single highest-impact area — I will
  pre-write the test matrix so it runs the day it lands.
