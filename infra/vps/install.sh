#!/usr/bin/env bash
# Copyright Morgan Metz
# Licensed under the PolyForm Noncommercial License 1.0.0.
# See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

# Wheel on an Ubuntu 24.04 server without Docker: built from source, run by systemd, behind Caddy.
# Idempotent and non-interactive: every input is a flag or an environment variable, nothing reads
# stdin, and running it again moves to another --ref or changes a setting.
#
#   sudo ./install.sh --domain wheel.example.com [--email you@example.com]   HTTPS (Let's Encrypt)
#   sudo ./install.sh --no-proxy                                            loopback only (SSH tunnel)
#
#   --repo <url|path>  what to clone          (default https://github.com/Morgandri1/wheel.git)
#   --ref <ref>        branch, tag or commit  (default main)
#   --tls-internal     with --domain: Caddy's own CA instead of Let's Encrypt
#   --updatable        let a running wheeld replace itself (auto-update lane); see README.md
#   --firewall         allow 22, 80 and 443 in ufw, and enable it
#   --dry-run          resolve settings and the target commit, print every step it would take,
#                       change nothing on disk and touch no package manager, service or firewall
#
# There is no plain-HTTP-publicly mode, on purpose, matching compose.yml. An earlier version had
# one (--public-host): Caddy would adapt `:80` with no host matcher, so the HSTS route's
# `protocol https` match never fired, and WHEEL_PUBLIC_ORIGIN=http://<host> meant the session
# cookie lost both Secure and the __Host- prefix. A password does not get a second chance once it
# has crossed the network once in the clear — see README.md and preflight.sh's compose-side
# reasoning, which applies here unchanged.
#
# Layout. src and bin are the auto-update hook points (WHEEL_UPDATE_REPO, WHEEL_UPDATE_BIN_DIR):
#   /opt/wheel/src   git checkout          /opt/wheel/bin    wheeld, wheel
#   /opt/wheel/web   web app server        /opt/wheel/rust   shared Rust toolchain
#   /var/lib/wheel   data, 0700, `wheel`   /var/cache/wheel  build caches, `wheel-build`
#   /etc/wheel       settings              units: wheeld, wheel-web, caddy

set -euo pipefail

repo=https://github.com/Morgandri1/wheel.git
ref=main
domain=""
no_proxy=0
email=""
tls_internal=0
updatable=0
firewall=0
dry_run=0

die() {
    echo "install: $*" >&2
    exit 1
}
step() {
    echo "==> $*"
}

# Wraps a mutating command. In a dry run it prints the command, shell-quoted, and does nothing;
# otherwise it runs it. Read-only commands (id checks, git fetch/ls-remote, version probes) are
# not wrapped, because a dry run's whole point is to resolve and REPORT real state honestly.
run() {
    if [ "$dry_run" = 1 ]; then
        printf '   (dry-run) would run:'
        printf ' %q' "$@"
        printf '\n'
        return 0
    fi
    "$@"
}

# `write()`'s job is a file with exact content and mode; in a dry run it reports the target and
# consumes stdin so a caller's pipeline does not block or error on a closed reader.
write() {
    local path=$1 mode=$2
    if [ "$dry_run" = 1 ]; then
        cat >/dev/null
        echo "   (dry-run) would write $path (mode $mode)"
        return 0
    fi
    (umask 077 && cat >"$path.new")
    chmod "$mode" "$path.new"
    mv -f "$path.new" "$path"
}

# A private, 0700 directory for anything downloaded before it's verified or installed — never a
# fixed /tmp path. A predictable, world-writable path that root then executes or imports (the
# NodeSource script, the rustup installer, Caddy's signing key) is a race another local user can
# win between the download and the run.
install_tmp="$(mktemp -d)"
chmod 0700 "$install_tmp"
trap 'rm -rf "$install_tmp"' EXIT

while [ $# -gt 0 ]; do
    case "$1" in
        --domain) domain="${2:?--domain needs a name}"; shift 2 ;;
        --public-host) die "--public-host was removed: it served plain HTTP publicly, which is never a supported mode (see the top of this script). Use --domain for HTTPS, or --no-proxy plus an SSH tunnel." ;;
        --no-proxy) no_proxy=1; shift ;;
        --email) email="${2:?--email needs an address}"; shift 2 ;;
        --repo) repo="${2:?--repo needs a URL or path}"; shift 2 ;;
        --ref) ref="${2:?--ref needs a branch, tag or commit}"; shift 2 ;;
        --tls-internal) tls_internal=1; shift ;;
        --updatable) updatable=1; shift ;;
        --firewall) firewall=1; shift ;;
        --dry-run) dry_run=1; shift ;;
        -h | --help) sed -n '6,27p' "$0"; exit 0 ;;
        *) die "unknown argument $1 (see --help)" ;;
    esac
done

modes=0
[ -z "$domain" ] || modes=$((modes + 1))
[ "$no_proxy" = 0 ] || modes=$((modes + 1))
[ "$modes" = 1 ] || die "choose exactly one of --domain and --no-proxy"
[ "$tls_internal" = 0 ] || [ -n "$domain" ] || die "--tls-internal needs --domain"
[ "$(id -u)" = 0 ] || die "run as root (sudo)"
# shellcheck source=/dev/null
. /etc/os-release
[ "${ID:-}" = ubuntu ] && [ "${VERSION_ID:-}" = 24.04 ] || die "this installer targets Ubuntu 24.04, not ${PRETTY_NAME:-this system}"

if [ -n "$domain" ]; then
    origin="https://$domain"
    api_base="$origin"
    site="$domain"
    allowed_hosts="$domain"
    mode_label="domain($domain)"
else
    origin="http://localhost:3000"
    api_base="http://localhost:8080"
    site=""
    allowed_hosts=""
    mode_label="no-proxy"
fi

[ "$dry_run" = 0 ] || echo "==> DRY RUN: resolving settings and the target commit; nothing on this machine will change"
echo "    mode=$mode_label ref=$ref repo=$repo updatable=$updatable firewall=$firewall"

src=/opt/wheel/src
vps="$src/infra/vps"
git_src() {
    git -c safe.directory="$src" -C "$src" "$@"
}
as_builder() {
    runuser -u wheel-build -- env -i HOME=/var/cache/wheel PATH=/opt/wheel/rust/cargo/bin:/usr/local/bin:/usr/bin:/bin \
        RUSTUP_HOME=/opt/wheel/rust/rustup CARGO_HOME=/var/cache/wheel/cargo "$@"
}

step "system packages"
export DEBIAN_FRONTEND=noninteractive
run apt-get update -q
# build-essential is load-bearing: rusqlite's bundled sqlite and ring's asm both need a C
# compiler, or the cargo build below fails deep in a dependency with an error that reads like a
# Rust problem. pkg-config and libssl-dev are NOT — this workspace is rustls-only (no openssl-sys
# in Cargo.lock at all) — but are installed anyway for parity with docker/Dockerfile.host, which
# has the same follow-up recorded to drop them once confirmed unneeded there too.
run apt-get install -y -q --no-install-recommends \
    ca-certificates curl git gnupg build-essential pkg-config libssl-dev make python3 python3-venv procps
if ! node_version="$(node --version 2>/dev/null)" || [ "${node_version%%.*}" != v22 ]; then
    step "Node.js 22 from NodeSource"
    if [ "$dry_run" = 1 ]; then
        echo "   (dry-run) would install Node.js 22 from NodeSource"
    else
        curl -fsSL https://deb.nodesource.com/setup_22.x -o "$install_tmp/nodesource_setup.sh"
        bash "$install_tmp/nodesource_setup.sh"
        apt-get install -y -q nodejs
    fi
fi
run npm install -g --no-fund --no-audit --loglevel=error pnpm@9.15.4 @anthropic-ai/claude-code @openai/codex

step "users and directories"
id wheel >/dev/null 2>&1 || run useradd --system --home-dir /var/lib/wheel --no-create-home --shell /usr/sbin/nologin wheel
id wheel-build >/dev/null 2>&1 || run useradd --system --home-dir /var/cache/wheel --no-create-home --shell /usr/sbin/nologin wheel-build
run install -d -m 0700 -o wheel -g wheel /var/lib/wheel
run install -d -m 0755 -o wheel-build -g wheel-build /var/cache/wheel
run install -d -m 0755 -o root -g root /opt/wheel /opt/wheel/bin /opt/wheel/rust /etc/wheel

export RUSTUP_HOME=/opt/wheel/rust/rustup
if [ -x /opt/wheel/rust/cargo/bin/rustup ]; then
    step "Rust toolchain: update"
    run env CARGO_HOME=/opt/wheel/rust/cargo /opt/wheel/rust/cargo/bin/rustup update stable --no-self-update
else
    step "Rust toolchain: install (shared, read-only to the services)"
    if [ "$dry_run" = 1 ]; then
        echo "   (dry-run) would install rustup + stable toolchain under /opt/wheel/rust"
    else
        curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs -o "$install_tmp/rustup-init.sh"
        CARGO_HOME=/opt/wheel/rust/cargo sh "$install_tmp/rustup-init.sh" -y --no-modify-path --profile minimal \
            --default-toolchain stable --component clippy --component rustfmt
    fi
fi
run chmod -R a+rX /opt/wheel/rust

step "source: $repo at $ref"
if [ -d "$src/.git" ]; then
    run git_src remote set-url origin "$repo"
    run git_src fetch --quiet --prune --tags --force origin
    commit="$(git_src rev-parse --verify --quiet "origin/$ref^{commit}" || git_src rev-parse --verify --quiet "$ref^{commit}")" ||
        die "no branch, tag or commit '$ref' in $repo"
elif [ "$dry_run" = 1 ]; then
    # No local checkout yet, and a dry run must not create one. On a genuinely fresh box, git
    # itself is not installed yet either (that install is one of the steps being skipped), so this
    # degrades honestly instead of crashing on a missing command.
    if ! command -v git >/dev/null 2>&1; then
        commit="unknown (git is not installed yet on this machine; a real run installs it first, then resolves $ref)"
    else
        # ls-remote resolves a branch or tag name without cloning. It cannot resolve a bare commit
        # sha the repo has not advertised a ref for, which a real run can (git fetch pulls it) —
        # that gap exists only before the first clone.
        commit="$(git ls-remote "$repo" "refs/heads/$ref" "refs/tags/$ref" | awk 'NR==1{print $1}')"
        [ -n "$commit" ] || commit="$ref (could not resolve from $repo without cloning; a real run would fetch and try as a commit sha)"
    fi
else
    run git clone --quiet "$repo" "$src"
    run git_src fetch --quiet --prune --tags --force origin
    commit="$(git_src rev-parse --verify --quiet "origin/$ref^{commit}" || git_src rev-parse --verify --quiet "$ref^{commit}")" ||
        die "no branch, tag or commit '$ref' in $repo"
fi
echo "    target commit: $commit"
if [ "$dry_run" = 0 ]; then
    git_src -c advice.detachedHead=false checkout --quiet --force --detach "$commit"
    git_src clean -ffdxq
else
    echo "   (dry-run) would checkout --detach $commit and clean the working tree"
fi

step "building wheeld and wheel at $commit (as wheel-build)"
if [ "$dry_run" = 1 ]; then
    echo "   (dry-run) would build --release --locked --bin wheeld --bin wheel and install them to /opt/wheel/bin"
else
    as_builder env CARGO_TARGET_DIR=/var/cache/wheel/target WHEEL_BUILD_SHA="$commit" \
        cargo build --release --locked --quiet --manifest-path "$src/Cargo.toml" --bin wheeld --bin wheel
    for bin in wheeld wheel; do
        install -m 0755 "/var/cache/wheel/target/release/$bin" "/opt/wheel/bin/.$bin.new"
        mv -f "/opt/wheel/bin/.$bin.new" "/opt/wheel/bin/$bin"
        ln -sfn "/opt/wheel/bin/$bin" "/usr/local/bin/$bin"
    done
    /opt/wheel/bin/wheeld --version
fi

step "building the web app (as wheel-build)"
if [ "$dry_run" = 1 ]; then
    echo "   (dry-run) would build the standalone web app from $commit and install it to /opt/wheel/web"
else
    rm -rf /var/cache/wheel/web
    install -d -o wheel-build -g wheel-build /var/cache/wheel/web
    git_src archive "$commit" web | runuser -u wheel-build -- tar -x -C /var/cache/wheel/web --strip-components=1
    as_builder sh -c 'cd /var/cache/wheel/web && pnpm install --frozen-lockfile --prod=false --reporter=silent && WHEEL_STANDALONE=1 NEXT_TELEMETRY_DISABLED=1 pnpm build'
    [ -f /var/cache/wheel/web/.next/standalone/server.js ] || die "the web build produced no .next/standalone/server.js"
    rm -rf /opt/wheel/web.new
    cp -a /var/cache/wheel/web/.next/standalone /opt/wheel/web.new
    cp -a /var/cache/wheel/web/.next/static /opt/wheel/web.new/.next/static
    [ ! -d /var/cache/wheel/web/public ] || cp -a /var/cache/wheel/web/public /opt/wheel/web.new/public
    rm -rf /opt/wheel/web.new/.next/cache
    ln -s /var/cache/wheel-web /opt/wheel/web.new/.next/cache
    chown -R root:root /opt/wheel/web.new
    chmod -R a+rX,go-w /opt/wheel/web.new
    rm -rf /opt/wheel/web.prev
    [ ! -d /opt/wheel/web ] || mv /opt/wheel/web /opt/wheel/web.prev
    mv /opt/wheel/web.new /opt/wheel/web
    rm -rf /opt/wheel/web.prev
fi

step "settings in /etc/wheel (yours go in *.local.env, which this never touches)"
{
    echo "PUBLIC_BASE_URL=$api_base"
    [ -z "$allowed_hosts" ] || echo "WHEEL_ALLOWED_HOSTS=$allowed_hosts"
    [ "$no_proxy" = 1 ] || echo "WHEEL_TRUSTED_PROXIES=127.0.0.1/32,::1"
    echo "WHEEL_SIGNUP=closed"
    echo "RUST_LOG=info"
    echo "PATH=/opt/wheel/rust/cargo/bin:/usr/local/bin:/usr/bin:/bin"
    echo "RUSTUP_HOME=/opt/wheel/rust/rustup"
} | write /etc/wheel/wheeld.env 0640
{
    echo "WHEEL_API_URL=http://127.0.0.1:8080"
    echo "WHEEL_AUTH_MODE=local"
    echo "WHEEL_PUBLIC_ORIGIN=$origin"
    if [ "$no_proxy" = 1 ]; then echo "WHEEL_TRUST_PROXY=0"; else echo "WHEEL_TRUST_PROXY=1"; fi
} | write /etc/wheel/web.env 0640

owner=root
[ "$updatable" = 0 ] || owner=wheel
run chown -R "$owner:$owner" "$src" /opt/wheel/bin
run install -d -m 0755 /etc/systemd/system/wheeld.service.d
if [ "$updatable" = 1 ]; then
    run install -d -m 0700 -o wheel -g wheel /var/cache/wheel/update
    write /etc/systemd/system/wheeld.service.d/auto-update.conf 0644 <<'EOF'
# Written by install.sh --updatable: the paths the auto-update lane's source driver works in.
# The policy itself (WHEEL_AUTO_UPDATE, default off) belongs in /etc/wheel/wheeld.local.env.
[Service]
ReadWritePaths=/opt/wheel/src /opt/wheel/bin /var/cache/wheel/update
Environment=WHEEL_UPDATE_REPO=/opt/wheel/src WHEEL_UPDATE_BIN_DIR=/opt/wheel/bin WHEEL_UPDATE_STAGING=/var/cache/wheel/update
EOF
elif [ "$dry_run" = 0 ]; then
    rm -f /etc/systemd/system/wheeld.service.d/auto-update.conf
else
    echo "   (dry-run) would remove /etc/systemd/system/wheeld.service.d/auto-update.conf if present"
fi

step "systemd units"
run install -m 0644 "$vps/systemd/wheeld.service" /etc/systemd/system/wheeld.service
run install -m 0644 "$vps/systemd/wheel-web.service" /etc/systemd/system/wheel-web.service
run systemctl daemon-reload
run systemctl enable --quiet wheeld wheel-web
run systemctl restart wheeld

# wheel-web and Caddy start only once wheeld is proven to enforce its own signup gate — a config
# value is not a control until the binary that reads it is proven to. Without this, a broken
# build worked around by editing wheeld.service's ExecStart to point at some other binary would
# still show WHEEL_SIGNUP=closed in the env file and nothing else would notice, and in --no-proxy
# mode there is no Caddy in front to fall back on.
if [ "$dry_run" = 1 ]; then
    echo "   (dry-run) would wait for wheeld's healthz, then verify it enforces WHEEL_SIGNUP before starting wheel-web or Caddy"
else
    step "waiting for wheeld"
    healthy=0
    for _ in $(seq 1 60); do
        if curl -fsS -o /dev/null http://127.0.0.1:8080/healthz; then
            healthy=1
            break
        fi
        sleep 1
    done
    [ "$healthy" = 1 ] || die "wheeld did not answer on 127.0.0.1:8080: journalctl -u wheeld -n 50"

    step "verifying wheeld enforces its own signup gate"
    # install.sh always writes WHEEL_SIGNUP=closed to wheeld.env; the only way it becomes "open" is
    # the operator's own wheeld.local.env, read second so it wins — mirror that resolution order
    # here rather than assuming the default this script wrote is still the effective one.
    effective_signup=closed
    if [ -f /etc/wheel/wheeld.local.env ] && grep -q '^WHEEL_SIGNUP=' /etc/wheel/wheeld.local.env; then
        effective_signup="$(grep '^WHEEL_SIGNUP=' /etc/wheel/wheeld.local.env | tail -1 | cut -d= -f2-)"
    fi
    signup_body='{"email":"verify-signup-gate@wheel.invalid","password":"verify-signup-gate-probe-not-a-real-account"}'
    signup_status="$(curl -sS -m 10 -o "$install_tmp/signup-check-body" -w '%{http_code}' -X POST http://127.0.0.1:8080/v1/auth/signup \
        -H 'content-type: application/json' -d "$signup_body")" || die "could not reach wheeld's own signup route at all — it is not answering as itself"
    signup_body_text="$(cat "$install_tmp/signup-check-body")"
    case "$effective_signup" in
        open)
            if [ "$signup_status" = 403 ] && printf '%s' "$signup_body_text" | grep -q '"forbidden"'; then
                die "WHEEL_SIGNUP=open but wheeld still answers 403 forbidden — signup is not actually open"
            fi
            ;;
        *)
            if [ "$signup_status" != 403 ] || ! printf '%s' "$signup_body_text" | grep -q '"forbidden"'; then
                die "WHEEL_SIGNUP=closed but POST /v1/auth/signup answered $signup_status, not the documented 403 {\"error\":{\"code\":\"forbidden\"}} — refusing to start wheel-web or Caddy in front of a wheeld that does not enforce its own signup gate"
            fi
            ;;
    esac
fi
run systemctl restart wheel-web

if [ "$no_proxy" = 0 ]; then
    if ! command -v caddy >/dev/null; then
        step "Caddy from its official apt repository"
        if [ "$dry_run" = 1 ]; then
            echo "   (dry-run) would add Caddy's apt repository and install caddy"
        else
            apt-get install -y -q debian-keyring debian-archive-keyring apt-transport-https
            curl -1sLf https://dl.cloudsmith.io/public/caddy/stable/gpg.key -o "$install_tmp/caddy-stable.gpg.key"
            gpg --dearmor --yes -o /usr/share/keyrings/caddy-stable-archive-keyring.gpg "$install_tmp/caddy-stable.gpg.key"
            curl -1sLf https://dl.cloudsmith.io/public/caddy/stable/debian.deb.txt -o /etc/apt/sources.list.d/caddy-stable.list
            chmod o+r /usr/share/keyrings/caddy-stable-archive-keyring.gpg /etc/apt/sources.list.d/caddy-stable.list
            apt-get update -q
            apt-get install -y -q caddy
        fi
    fi
    step "Caddy configuration"
    email_option=""
    [ -z "$email" ] || email_option="email $email"
    tls_option=""
    [ "$tls_internal" = 0 ] || tls_option="tls internal"
    run install -m 0644 "$vps/Caddyfile" /etc/caddy/Caddyfile
    {
        echo "CADDY_SITE=$site"
        echo "CADDY_EMAIL_OPTION=$email_option"
        echo "CADDY_TLS_OPTION=$tls_option"
        echo "WHEEL_API_UPSTREAM=127.0.0.1:8080"
        echo "WHEEL_WEB_UPSTREAM=127.0.0.1:3000"
        echo "WHEEL_SIGNUP=closed"
    } | write /etc/wheel/caddy.env 0640
    run install -d -m 0755 /etc/systemd/system/caddy.service.d
    printf '[Service]\nEnvironmentFile=/etc/wheel/caddy.env\n' | write /etc/systemd/system/caddy.service.d/wheel.conf 0644
    if [ "$dry_run" = 1 ]; then
        echo "   (dry-run) would validate $vps/Caddyfile with 'caddy validate'"
    else
        runuser -u caddy -- env HOME=/var/lib/caddy CADDY_SITE="$site" CADDY_EMAIL_OPTION="$email_option" \
            CADDY_TLS_OPTION="$tls_option" WHEEL_API_UPSTREAM=127.0.0.1:8080 WHEEL_WEB_UPSTREAM=127.0.0.1:3000 \
            WHEEL_SIGNUP=closed caddy validate --config /etc/caddy/Caddyfile --adapter caddyfile
    fi
    run systemctl daemon-reload
    run systemctl enable --quiet caddy
    run systemctl restart caddy
elif systemctl list-unit-files caddy.service >/dev/null 2>&1; then
    run systemctl disable --now --quiet caddy || true
fi

if [ "$firewall" = 1 ]; then
    step "ufw"
    run ufw allow 22/tcp
    if [ "$no_proxy" = 0 ]; then
        run ufw allow 80/tcp
        run ufw allow 443/tcp
        run ufw allow 443/udp
    fi
    run ufw --force enable
fi

if [ "$dry_run" = 1 ]; then
    echo
    echo "DRY RUN complete. Nothing on this machine was changed."
    exit 0
fi

cat <<EOF

Wheel $commit is running.
  board        $origin
  API          $api_base   (AgentGrid, curl: send a wht_ token)
  data         /var/lib/wheel  (master.key and operator-token live here: back it up, encrypted)
  operator     sudo cat /var/lib/wheel/operator-token   (then add your account: see README.md)
  logs         journalctl -u wheeld -u wheel-web -u caddy
EOF
if [ "$firewall" = 0 ]; then
    echo "  firewall     not touched. Suggested: ufw allow 22/tcp; ufw allow 80/tcp; ufw allow 443/tcp; ufw enable"
fi
