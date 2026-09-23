# Setup

Everything beyond the README's fastest path: building from source, the board UI in depth, production
deployment, and every environment variable that shapes how `wheeld` runs.

## Building `wheeld` from source

Needs: Rust stable 1.88+ ([rustup.rs](https://rustup.rs) — not your distro's packaged `rustc`, it's usually too
old), a C compiler (`build-essential` on Debian/Ubuntu, Xcode Command Line Tools on macOS), and `git` (also used
at runtime, for agent workspaces backed by a repo). Building doesn't need Node — but running an agent does:
`npm install -g @anthropic-ai/claude-code @openai/codex`, or use Docker, which bundles both.

```bash
cargo build --release -p wheeld
./target/release/wheeld          # API + sandbox host + agents in one process, sqlite store, on http://127.0.0.1:8080
```

- Listens on `127.0.0.1:8080` by default. `--bind` / `BIND_ADDR` changes that; any address other people can reach
  logs a warning saying so.
- Everything lives in `~/.wheel` (`--data-dir` / `WHEEL_DATA_DIR`): the store, `master.key`, every project. The
  directory is `0700` — treat it like an SSH key.
- First start writes an operator token to `~/.wheel/operator-token` (`0600`). The log shows the path, never the
  token.

## Tokens

```bash
wheeld token create --name laptop         # prints the new token, on stdout, this once
wheeld token list                         # id, account, name, created, last used, revoked
wheeld token revoke <id>                  # revokes it and every token it minted
```

Tokens are managed against the data directory itself — whoever can read it can already read `master.key`, so
this is no new power. Any signed-in client can also mint its own over HTTP (`POST /v1/auth/tokens`, in
`docs/API.md`).

## Shutdown

SIGTERM or ctrl-c lets turns in flight finish (up to 20s), then stops every project's agents and their process
groups before `wheeld` exits; allow 30s. A process an agent deliberately detaches (`setsid`) is beyond that.

As a systemd service, use `KillMode=mixed`, not the default `control-group`: `mixed` sends SIGTERM to `wheeld`
alone, so it runs its own drain and stops its agents itself, rather than systemd signalling every agent process
at the same instant `wheeld` gets its own signal — which would kill a turn already running mid-flight.
`TimeoutStopSec` is the backstop: past it, systemd SIGKILLs the whole cgroup, which is what still cleans up a
detached (`setsid`) process `wheeld` itself can no longer reach.

```ini
[Service]
ExecStart=/usr/local/bin/wheeld --data-dir /var/lib/wheel
KillMode=mixed
TimeoutStopSec=30
```

## Signup

`WHEEL_SIGNUP=open|closed` (closed is the default). An account's agents run as your user, so a stranger's signup
is a stranger's code on your box — open it only where nobody else can reach it. While closed, the owner adds
people directly, keeping the password off the command line:

```bash
printf '{"email":"%s","password":"%s"}' you@example.com "$PASSWORD" | wh /v1/auth/users -d @-
```

## Bringing your own identity provider (`AUTH_MODE=external`)

`wheeld`'s built-in accounts are the default and need nothing here. If you already run an identity
system — Keycloak, Authentik, Dex, Zitadel, Okta, Auth0, Entra, Cloudflare Access, or an in-house
signer that publishes a JWKS — Wheel can verify *its* tokens instead. The full contract, every
variable and every boot refusal is in `docs/API.md`; this is the operator's version.

Nothing about this unifies accounts. Wheel maps a verified foreign subject to a Wheel account it
mints itself, and everything downstream — ownership, membership, attribution — keys off that.

**A JWKS issuer.** The recommended shape, and the only one with no availability coupling and no new
secret for Wheel to hold:

```bash
AUTH_MODE=external
WHEEL_EXTERNAL_VERIFIER=jwks
WHEEL_EXTERNAL_ISSUER=https://accounts.example.com          # exactly the `iss` your tokens carry
WHEEL_EXTERNAL_JWKS_URL=https://accounts.example.com/jwks
WHEEL_EXTERNAL_ALGS=RS256,EdDSA                             # what your issuer actually signs with
WHEEL_EXTERNAL_AUDIENCE=https://wheel.example.com           # what WE are — see below
WHEEL_EXTERNAL_PROVISION=linked                             # or `auto`; there is no default
```

Four of those deserve a sentence each, because getting one wrong is the difference between a
credential and a doorway:

- **`WHEEL_EXTERNAL_AUDIENCE` must name this Wheel deployment and nothing else.** Not your issuer's
  origin. An issuer that serves several of its own surfaces usually mints for all of them under one
  issuer, and several of those already carry the issuer origin as their `aud` — set that here and
  every one of them becomes a valid Wheel login. Wheel warns at boot if you do; it cannot refuse,
  because it does not know what else your issuer serves.
- **`WHEEL_EXTERNAL_ALGS` is an allowlist, and it must not contain an `HS*` algorithm.** Wheel
  refuses one by name at boot: a key that verifies an HMAC is also a key that mints one.
- **`WHEEL_EXTERNAL_PROVISION` has no default and you must state it.** `auto` gives a Wheel account
  to anyone your issuer vouches for — right when the IdP's population *is* the intended Wheel
  population, and wrong when your IdP lets anyone sign up. `linked` refuses an unknown subject until
  an operator links it, and the first-boot operator token is the credential that does the first
  linking, so there is no chicken-and-egg:

  ```bash
  wh /v1/auth/users -d '{"email":"you@example.com","password":"…"}'   # or an existing account id
  wh /v1/auth/external-identities -d '{"subject":"<the sub your IdP issues>","user_id":"<uuid>"}'
  wh /v1/auth/external-identities                                     # list them
  wh /v1/auth/external-identities/<id> -X DELETE                      # disable one
  ```

  Those three routes need the operator account (the one `wheeld` writes the first token for) and
  `404` on a deployment that is not running `external`.
- **Your IdP must never reuse a `sub`.** OIDC requires it, and not every implementation obeys. A
  reassigned subject inherits the previous human's Wheel account, projects and memberships, and
  Wheel cannot detect it. If your IdP publishes a better immutable id — `oid` on Entra, `user_id` on
  several others — point `WHEEL_EXTERNAL_SUBJECT_CLAIM` at that instead of `sub`.

**Cloudflare Access** is a JWKS deployment, not a header one. It signs its assertion and publishes a
key set, so verify it rather than trusting it: keep `WHEEL_EXTERNAL_VERIFIER=jwks` and add
`WHEEL_EXTERNAL_TOKEN_HEADER=cf-access-jwt-assertion`.

**A reverse proxy that has already authenticated the user** (oauth2-proxy, Pomerium, `nginx
auth_request`, Tailscale serve) is the other verifier, and it is the dangerous one:

```bash
AUTH_MODE=external
WHEEL_EXTERNAL_VERIFIER=proxy_header
WHEEL_EXTERNAL_ISSUER=proxy:oauth2-proxy                    # a stable label; no token exists to carry one
WHEEL_EXTERNAL_AUDIENCE=wheel
WHEEL_EXTERNAL_PROVISION=linked
WHEEL_EXTERNAL_PROXY_SUBJECT_HEADER=x-forwarded-user
WHEEL_EXTERNAL_PROXY_EMAIL_HEADER=x-forwarded-email         # optional, display only
WHEEL_TRUSTED_PROXIES=10.0.0.5/32                           # REQUIRED here; empty OR 0.0.0.0/0 refuses to boot
```

Boot refuses this mode with an empty `WHEEL_TRUSTED_PROXIES`, and also with an all-addresses one
(`0.0.0.0/0`, `::/0`). That second refusal exists because the first one is easy to satisfy the wrong
way: an operator on a platform with no pinnable load-balancer address hits "empty refuses to boot"
and widens the range until it starts. If that is your platform, **this is not the verifier for that
deployment** — use `jwks`, which authenticates a signature instead of a network position.

Wheel verifies **nothing** about that header. The proxy is the verifier, so the entire control is
that the request reached Wheel *from* the proxy — which means **Wheel must not be reachable any
other way.** Not "should": anything that can open a TCP connection to Wheel directly can be anyone.
`WHEEL_TRUSTED_PROXIES` is the last check, not the only one; put Wheel on a network the proxy is the
only route into. Setting it to `127.0.0.1` trusts every process on the machine, agents included.

Wheel adds two things on top, and both are on by default in this mode: a **cross-origin request is
refused 403** (the credential is ambient, so a hostile page could otherwise spend it from a victim's
browser — set `CORS_ALLOWED_ORIGINS` if a browser client genuinely needs to call the API directly),
and the subject and email headers are **stripped before anything is forwarded to an engine**, so an
agent can never read or replay who the edge said was calling.

**Trying it without an identity provider.** `cargo run -p wheel-api --example stub-issuer` serves a
key set with both an RSA and an Ed25519 key and prints a ready token for each mode. It needs
`WHEEL_ENV=dev`, because the production interlock refuses a loopback issuer — a stub issuer
authenticates everyone as anyone. `docs/API.md` has the full recipe.

**One thing external auth does not get.** `POST /v1/auth/tokens` is refused for an
externally-authenticated caller. Your IdP's token is short-lived and you can revoke it; a `wht_`
token is neither, so trading one for the other would hand out an indefinite credential your identity
system can no longer take away. A `wht_` token *minted some other way* still works normally in every
mode, including behind a proxy — that is how `wheeld token` keeps working from a script.

## Network and CORS

The browser-facing settings are closed by default: `CORS_ALLOWED_ORIGINS` is empty, and a request is refused
unless its `Host` is an IP address, `localhost`, the bind address, or a name in `WHEEL_ALLOWED_HOSTS` — that's
what stops a web page reaching `wheeld` through DNS rebinding.

**Behind a reverse proxy** (the VPS kit in `infra/vps/` sets all of these):
- `PUBLIC_BASE_URL=https://<domain>` — ingress URLs and sessions name the public address.
- `WHEEL_TRUSTED_PROXIES=<proxy address>` — `X-Forwarded-For` is believed from this address and no one else.
  Trusting `127.0.0.1` trusts every local process, agents included; the Docker layout trusts the proxy's
  container.
- `WHEEL_ALLOWED_HOSTS=<domain>` — the proxy passes the public `Host` through, and it needs to be admitted.

## Docker, in depth

Needs: Docker Engine with the Compose v2 plugin (`docker compose version` should print `v2.x`; Ubuntu's `docker.io`
apt package is often too old — use Docker's own repository) and `git`, to get this repo (the image is built from
it, not pulled).

```bash
docker build -f docker/Dockerfile.wheeld -t wheeld .              # or: make wheeld-image
docker run -d --name wheeld --stop-timeout 30 -v wheel-data:/data -p 127.0.0.1:8080:8080 wheeld
(umask 077; docker exec wheeld cat /data/operator-token > ~/.wheel-token)
export WHEEL_TOKEN_FILE=~/.wheel-token                             # then `wh` as in the README
docker exec wheeld wheeld token create --name ci                   # more tokens, the same way
```
The same thing as a compose file: `docker compose -f infra/compose.wheeld.yml up -d --build`.

- One non-root image: `wheeld`, the `wheel` CLI, `claude`, `codex`, and a development toolchain. `/data` is its
  volume.
- Inside the container `wheeld` listens on `0.0.0.0`, which is only the container's own network — publish it on
  `127.0.0.1` only, as above.
- `docker stop` sends SIGTERM to `wheeld` (PID 1 is `tini`, which forwards it) and waits out its stop timeout.
  Docker's own default is 10s, too short for the drain above — that's why `--stop-timeout 30` is in the
  `docker run` example and `stop_grace_period: 30s` is in `infra/compose.wheeld.yml`.

## The board UI, in depth

Needs: Node.js 22.x (`npx` ships with it). `wheel-web` is a prebuilt package, not a build from source.

```bash
WHEEL_API_URL=http://127.0.0.1:8080 npx wheel-web                                # against wheeld on this machine
docker compose -f infra/compose.wheeld.yml --profile web up -d --build           # or both in compose: UI on http://127.0.0.1:3000
```

The UI signs in with email and password. Signup is closed by default, so the owner adds your account with
`POST /v1/auth/users` (above); on a machine only you use, `WHEEL_SIGNUP=open` lets you sign up on the login page
instead. The UI calls the API from its own server, and the browser never talks to the API directly.

Projects belong to the account that created them. To script the boards you use in the UI, mint a token for that
account, either from the UI or with `wheeld token create --email you@example.com`.

## On your own cloud

Nothing here is built from source by you — the images are what's built, per the Docker section's prerequisites,
wherever you build or pull them.

- **Railway**: needs a Railway account and the `railway` CLI. Fork this repo, create services from
  `docker/Dockerfile.api` and `docker/Dockerfile.host` (+ Postgres), apply `infra/railway/settings.json` with
  `infra/railway/apply-settings.sh`; env vars are listed in `infra/railway/README.md` and `web/DEPLOY.md`. The
  host runs agents as per-project unix users on one machine (no Docker daemon needed) — size it for your agents'
  builds.
- **Any VM / Kubernetes**: needs Docker (or a way to run OCI images) plus your own Postgres. Run the two images
  with Postgres; the host needs a persistent volume at `/data` and must NOT be publicly reachable (the API talks
  to it over a private network with `WHEEL_HOST_SECRET`). The web app is a standard Next.js server that reaches
  the API at `WHEEL_API_URL`.
- API tokens work on the cloud API too, whatever identity provider signs its sessions, so a desktop or CI client
  authenticates the same way everywhere.

## Developing Wheel: the multi-service stack

Before this: Docker Engine with the Compose v2 plugin, `git` — Postgres, the API and the host all come from the
images built below, not from anything installed by hand.

```bash
docker network create wheel
docker compose -f infra/docker-compose.yml up --build              # postgres + api + host, API on 127.0.0.1:8080
docker compose -f infra/docker-compose.yml --profile web up --build   # plus the board UI on 127.0.0.1:3000
```

## Signing an agent in with OAuth instead of an API key

The README's quickstart writes a plain `ANTHROPIC_API_KEY` into the vault. To sign an agent into a real
Anthropic account instead (paste-code OAuth) — the credential a vault-sharing second agent can reuse with no
login of its own — extend that flow like this, continuing from the same `P`/`wh`/`id` helpers:

```bash
wh /v1/projects/$P/board/apply -d '{"board": {
  "nodes": [ {"name": "reviewer", "type": "agent", "config": {"harness": "claude", "system_prompt": "Review worker."}} ],
  "wires": [ {"from": "reviewer", "to": "keys", "type": "read"} ] }, "allow_wire": true}'

begin=$(wh /v1/projects/$P/engine/v1/agents/$(id worker)/auth/begin -X POST)
echo "$begin" | jq -r .url            # open this, sign in, and copy the code Anthropic shows you
session=$(echo "$begin" | jq -r .session)
read -rp 'paste the code: ' code
wh /v1/projects/$P/engine/v1/agents/$(id worker)/auth/complete -d "$(jq -n --arg c "$code" --arg s "$session" \
  '{code: $c, session: $s, save_to_vault: "keys", allow_shared_expiry: true}')"
```

Four traps in that flow, each one a refusal you'd otherwise have to reverse-engineer:
- The child spawned by `auth/begin` **stays alive between the two calls** — that's the whole reason
  this is two calls instead of one, since the CLI itself is what verifies the pasted code — and the
  engine kills it if you never come back with `auth/complete`.
- `save_to_vault` needs the agent to **already hold a `read` wire** to that vault (403 otherwise),
  which is why the wire is drawn in `board/apply` before any of this runs.
- `allow_shared_expiry: true` is **required whenever the credential expires and at least one other
  agent already reads that vault** — `reviewer` does here, so this would 409 (`shared_expiry`)
  without it: every reader stops the moment a shared session lapses, and a warning buried in a
  response body is the wrong place to learn that. `claude setup-token` (run locally, where the CLI
  is installed) produces a credential that never expires — paste it as
  `{"setup_token": "<token>", "save_to_vault": "keys"}` and the flag is not needed at all.
- **Never pass `vault_key`.** The engine derives the right variable name from the credential itself
  (`CLAUDE_CODE_OAUTH_TOKEN` for a login, `ANTHROPIC_API_KEY` for a provider key) and refuses a
  caller who names a different one — letting the caller choose would let one agent's key land under
  a name the harness doesn't read for every OTHER agent that shares the vault.

## Authoring a board from source

`board/apply`'s request shape is not what `GET .../board` hands back — one is a spec you write, the
other is board state with ids and runtime status — and mixing the two up is the most common way to
get a `422` here. Everything below is checked directly against `wheel_core::node`, `validate.rs` and
`wheel_core::wire::wire_allowed`, not transcribed from memory.

**Node shape**: `{"name", "type", "config": {...}, "position": {x, y}}` — `config` is a NESTED
object keyed by node type, never flattened onto the node itself (`NodeConfig` is adjacently
tagged: `#[serde(tag = "type", content = "config")]`). `position` defaults to `{0, 0}` if omitted.
Minimum config per type actually used above, plus the ones most often gotten wrong:
- `agent`: `harness` (`"claude"` or `"codex"`) and `system_prompt` are required; everything else
  (`model`, `run_on_startup`, `ephemeral_context`, `idle_timeout_secs`, `budget`, `workspaces`)
  defaults.
- `ctx`: `markdown` (a string) — **not** `text`.
- `vault`: `keys`, an array — `[]` is legal; it only documents what SHOULD be there and is never
  required to match what's actually written.
- `endpoint`: `method`, `path` (leading slash, no `..`), `response_mode` — exactly `"ack"` or
  `"script"`, nothing else. `auth` defaults to `{"mode": "none"}`.
- `table`: `columns`, an array of `{"name", "type"}` (`type` one of `text`/`integer`/`real`/`blob`/`json`).

**Wires** are a FLAT list in the request, `{"from", "to", "type"}`, addressed by NODE NAME — not
id. The matrix is asymmetric and default-deny; the two directions that trip people up most are
`vault → agent` (refused — vaults have no outgoing wires at all; an agent *reads* a vault, a vault
never reaches one) and anything `→ endpoint` (also refused — an endpoint only wires OUT). The rest,
transcribed from `wire_allowed` itself:

| from → to | read | write | send |
|---|---|---|---|
| agent → agent | — | — | ✓ |
| agent → ctx | ✓ | ✓ | — |
| agent → table | ✓ | ✓ | — |
| agent → vault | ✓ | — | — |
| agent → chest | ✓ | ✓ | — |
| agent → script | ✓ | — | — |
| agent → mcp | ✓ | — | — |
| agent → tool | ✓ | — | — |
| tool → vault | ✓ | — | — |
| ctx → agent | — | — | ✓ |
| endpoint → agent | — | — | ✓ |
| endpoint → table | — | ✓ | — |
| endpoint → script | — | — | ✓ |
| endpoint → vault | ✓ | — | — |
| script → agent | — | — | ✓ |
| script → ctx | ✓ | ✓ | — |
| script → table | ✓ | ✓ | — |
| script → chest | ✓ | ✓ | — |
| script → vault | ✓ | — | — |
| script → tool | ✓ | — | — |

Everything not in this table is refused. `mcp`, `table`, `vault` and `chest` have no outgoing wires
at all.

**`dry_run: true`** plans the board — same validation, same response shape — without creating or
wiring anything, so you can check a board before committing to it.

**`allow_patch`/`allow_wire`** default to `false`. A board that names a node which already exists
is refused (`422`, `patch_not_permitted`, naming every node it would have touched) rather than
silently modified; a wire that would attach to an existing node is refused the same way
(`wire_touches_existing_node`). Set the matching flag to opt in — the refusal body names exactly
what you're being asked to grant, so there's no guessing.

**The project-level HTTP gate is separate from the endpoint's own config, and easy to miss.** An
`endpoint` node with `auth: {"mode": "none"}` and a correct `path` still answers `403` on
`/p/{project}/...` until the PROJECT ITSELF has `capabilities.http: true`:
```bash
wh /v1/projects/$P -X PATCH -d '{"capabilities": {"http": true}}'
```
The endpoint can look completely correctly configured and still be refused — this gate is checked
before the request ever reaches the endpoint node, and nothing on the endpoint's own config hints
that it exists. Tracked as `redteam/findings/057` (the gate itself fails safe and is correct by
design; the finding is about the missing signal, not the gate).

**A saved `GET .../board` response is not what `board/apply` wants**, and feeding one straight to
the other fails confusingly rather than obviously: `GET .../board` returns wires PER NODE, keyed by
the OTHER node's id (`{"nodes": [{"id", "name", ..., "wires": [{"to": "<id>", "type": "read"}]}]}`);
`board/apply` wants one flat list keyed by NAME. Translating one into the other:
```bash
board=$(wh /v1/projects/$P/engine/v1/board)
by_id=$(echo "$board" | jq '[.nodes[] | {key: .id, value: .name}] | from_entries')
echo "$board" | jq --argjson by_id "$by_id" '{
  nodes: [.nodes[] | {name, type, config, position}],
  wires: [.nodes[] | .name as $from | .wires[] | {from: $from, to: $by_id[.to], type}]
}'
```
There is no built-in export/import endpoint yet to do this for you — it's tracked, not shipped.
