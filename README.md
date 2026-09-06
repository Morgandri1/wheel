# Wheel

They always say "don't reinvent the wheel" — sometimes you have to.

Wheel is a per-project, per-user container that runs Claude Code / Codex agents as child processes, wired to each
other and to tables, endpoints, scripts, MCP servers, vaults and chests on a visual board.

- `docs/ARCHITECTURE.md` — the shared contract every team builds against. Read it first.
- `docs/PROTOCOL.md` — engine control plane + `wheel` CLI (SDK/Engine team)
- `docs/API.md` — api.wheel.dev (API team)
- `docs/TESTPLAN.md` — acceptance criteria (QA)
- `redteam/` — threat model and findings (ADVERSARY)

Workflow: one worktree per team under `/Users/metatron/wheel-wt/<role>`, merge to `main` after `make check`.

## Running Wheel

Wheel is open source. You can run it three ways.

### 1. Locally (single executable) — M1.7, in progress
```bash
wheeld                       # API + host + sandboxed agents in one process, sqlite store, http://localhost:8080 — nothing else to install
npx wheel-web                # optional: the board UI against http://localhost:8080 (headless users skip this)
wheel --help                 # the agent-side CLI (also what your agents use inside their sandboxes)
```
Sign up at the API (`POST /v1/auth/signup`) or in the web app; auth is local email/password by default.

### 2. Locally with Docker (today)
```bash
export API_MASTER_KEY=$(openssl rand -base64 32) SESSION_SECRET=$(openssl rand -base64 32)
docker compose -f infra/docker-compose.yml up --build      # postgres + api + host (docker sandbox backend) on :8080
cd web && pnpm install && NEXT_PUBLIC_AUTH_MODE=local NEXT_PUBLIC_API_URL=http://localhost:8080 pnpm dev   # UI on :3000
```

### 3. On your own cloud
- **Railway**: fork this repo, create services from `docker/Dockerfile.api` and `docker/Dockerfile.host` (+ Postgres), apply
  `infra/railway/settings.json` with `infra/railway/apply-settings.sh`; env vars are listed in `infra/railway/README.md` and `web/DEPLOY.md`.
  The host runs agents as per-project unix users on one machine (no Docker daemon needed) — size it for your agents' builds.
- **Any VM / Kubernetes**: run the two images with Postgres; the host needs a persistent volume at `/data` and must NOT be publicly
  reachable (the API talks to it over a private network with `WHEEL_HOST_SECRET`). The web app is a standard Next.js app (Vercel, or `next start`).

### Agents and credentials
Agents are Claude Code / Codex processes. Give them credentials through a **vault** node (one vault per account; wire the agent to it) or the
agent's Authenticate panel (in-browser Anthropic login, `claude setup-token`, or an API key). Nothing in Wheel ever shows a stored secret back.
See `docs/ARCHITECTURE.md` for the model and `docs/WHEEL-ON-WHEEL.md` for a board that develops Wheel itself.
