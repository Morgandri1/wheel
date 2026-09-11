#!/usr/bin/env bash
# Copyright Morgan Metz
# Licensed under the PolyForm Noncommercial License 1.0.0.
# See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

# Wheel on an Ubuntu 24.04 server without Docker: built from source, run by systemd, behind Caddy.
# Idempotent: run it again to move to another --ref or to change a setting.
#
#   sudo ./install.sh --domain wheel.example.com [--email you@example.com]   HTTPS (Let's Encrypt)
#   sudo ./install.sh --public-host 203.0.113.5                             plain HTTP on :80
#   sudo ./install.sh --no-proxy                                            loopback only (SSH tunnel)
#
#   --repo <url|path>  what to clone          (default https://github.com/Morgandri1/wheel.git)
#   --ref <ref>        branch, tag or commit  (default main)
#   --tls-internal     with --domain: Caddy's own CA instead of Let's Encrypt
#   --updatable        let a running wheeld replace itself (auto-update lane); see README.md
#   --firewall         allow 22, 80 and 443 in ufw, and enable it
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
public_host=""
no_proxy=0
email=""
tls_internal=0
updatable=0
firewall=0

die() {
    echo "install: $*" >&2
    exit 1
}
step() {
    echo "==> $*"
}

while [ $# -gt 0 ]; do
    case "$1" in
        --domain) domain="${2:?--domain needs a name}"; shift 2 ;;
        --public-host) public_host="${2:?--public-host needs an address}"; shift 2 ;;
        --no-proxy) no_proxy=1; shift ;;
        --email) email="${2:?--email needs an address}"; shift 2 ;;
        --repo) repo="${2:?--repo needs a URL or path}"; shift 2 ;;
        --ref) ref="${2:?--ref needs a branch, tag or commit}"; shift 2 ;;
        --tls-internal) tls_internal=1; shift ;;
        --updatable) updatable=1; shift ;;
        --firewall) firewall=1; shift ;;
        -h | --help) sed -n '6,23p' "$0"; exit 0 ;;
        *) die "unknown argument $1 (see --help)" ;;
    esac
done

modes=0
[ -z "$domain" ] || modes=$((modes + 1))
[ -z "$public_host" ] || modes=$((modes + 1))
[ "$no_proxy" = 0 ] || modes=$((modes + 1))
[ "$modes" = 1 ] || die "choose exactly one of --domain, --public-host and --no-proxy"
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
elif [ -n "$public_host" ]; then
    origin="http://$public_host"
    api_base="$origin"
    site=":80"
    allowed_hosts="$public_host"
else
    origin="http://localhost:3000"
    api_base="http://localhost:8080"
    site=""
    allowed_hosts=""
fi

src=/opt/wheel/src
vps="$src/infra/vps"
git_src() {
    git -c safe.directory="$src" -C "$src" "$@"
}
as_builder() {
    runuser -u wheel-build -- env -i HOME=/var/cache/wheel PATH=/opt/wheel/rust/cargo/bin:/usr/local/bin:/usr/bin:/bin \
        RUSTUP_HOME=/opt/wheel/rust/rustup CARGO_HOME=/var/cache/wheel/cargo "$@"
}
write() {
    local path=$1 mode=$2
    (umask 077 && cat >"$path.new")
    chmod "$mode" "$path.new"
    mv -f "$path.new" "$path"
}

step "system packages"
export DEBIAN_FRONTEND=noninteractive
apt-get update -q
apt-get install -y -q --no-install-recommends \
    ca-certificates curl git gnupg build-essential pkg-config libssl-dev make python3 python3-venv procps
if ! node_version="$(node --version 2>/dev/null)" || [ "${node_version%%.*}" != v22 ]; then
    step "Node.js 22 from NodeSource"
    curl -fsSL https://deb.nodesource.com/setup_22.x -o /tmp/nodesource_setup.sh
    bash /tmp/nodesource_setup.sh
    apt-get install -y -q nodejs
fi
npm install -g --no-fund --no-audit --loglevel=error pnpm@9.15.4 @anthropic-ai/claude-code @openai/codex

step "users and directories"
id wheel >/dev/null 2>&1 || useradd --system --home-dir /var/lib/wheel --no-create-home --shell /usr/sbin/nologin wheel
id wheel-build >/dev/null 2>&1 || useradd --system --home-dir /var/cache/wheel --no-create-home --shell /usr/sbin/nologin wheel-build
install -d -m 0700 -o wheel -g wheel /var/lib/wheel
install -d -m 0755 -o wheel-build -g wheel-build /var/cache/wheel
install -d -m 0755 -o root -g root /opt/wheel /opt/wheel/bin /opt/wheel/rust /etc/wheel

export RUSTUP_HOME=/opt/wheel/rust/rustup
if [ -x /opt/wheel/rust/cargo/bin/rustup ]; then
    step "Rust toolchain: update"
    CARGO_HOME=/opt/wheel/rust/cargo /opt/wheel/rust/cargo/bin/rustup update stable --no-self-update
else
    step "Rust toolchain: install (shared, read-only to the services)"
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs -o /tmp/rustup-init.sh
    CARGO_HOME=/opt/wheel/rust/cargo sh /tmp/rustup-init.sh -y --no-modify-path --profile minimal \
        --default-toolchain stable --component clippy --component rustfmt
fi
chmod -R a+rX /opt/wheel/rust

step "source: $repo at $ref"
[ -d "$src/.git" ] || git clone --quiet "$repo" "$src"
git_src remote set-url origin "$repo"
git_src fetch --quiet --prune --tags --force origin
commit="$(git_src rev-parse --verify --quiet "origin/$ref^{commit}" || git_src rev-parse --verify --quiet "$ref^{commit}")" ||
    die "no branch, tag or commit '$ref' in $repo"
git_src -c advice.detachedHead=false checkout --quiet --force --detach "$commit"
git_src clean -ffdxq

step "building wheeld and wheel at $commit (as wheel-build)"
as_builder env CARGO_TARGET_DIR=/var/cache/wheel/target WHEEL_BUILD_SHA="$commit" \
    cargo build --release --locked --quiet --manifest-path "$src/Cargo.toml" --bin wheeld --bin wheel
for bin in wheeld wheel; do
    install -m 0755 "/var/cache/wheel/target/release/$bin" "/opt/wheel/bin/.$bin.new"
    mv -f "/opt/wheel/bin/.$bin.new" "/opt/wheel/bin/$bin"
    ln -sfn "/opt/wheel/bin/$bin" "/usr/local/bin/$bin"
done
/opt/wheel/bin/wheeld --version

step "building the web app (as wheel-build)"
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
chown -R "$owner:$owner" "$src" /opt/wheel/bin
install -d -m 0755 /etc/systemd/system/wheeld.service.d
if [ "$updatable" = 1 ]; then
    install -d -m 0700 -o wheel -g wheel /var/cache/wheel/update
    write /etc/systemd/system/wheeld.service.d/auto-update.conf 0644 <<'EOF'
# Written by install.sh --updatable: the paths the auto-update lane's source driver works in.
# The policy itself (WHEEL_AUTO_UPDATE, default off) belongs in /etc/wheel/wheeld.local.env.
[Service]
ReadWritePaths=/opt/wheel/src /opt/wheel/bin /var/cache/wheel/update
Environment=WHEEL_UPDATE_REPO=/opt/wheel/src WHEEL_UPDATE_BIN_DIR=/opt/wheel/bin WHEEL_UPDATE_STAGING=/var/cache/wheel/update
EOF
else
    rm -f /etc/systemd/system/wheeld.service.d/auto-update.conf
fi

step "systemd units"
install -m 0644 "$vps/systemd/wheeld.service" /etc/systemd/system/wheeld.service
install -m 0644 "$vps/systemd/wheel-web.service" /etc/systemd/system/wheel-web.service
systemctl daemon-reload
systemctl enable --quiet wheeld wheel-web
systemctl restart wheeld wheel-web

if [ "$no_proxy" = 0 ]; then
    if ! command -v caddy >/dev/null; then
        step "Caddy from its official apt repository"
        apt-get install -y -q debian-keyring debian-archive-keyring apt-transport-https
        curl -1sLf https://dl.cloudsmith.io/public/caddy/stable/gpg.key -o /tmp/caddy-stable.gpg.key
        gpg --dearmor --yes -o /usr/share/keyrings/caddy-stable-archive-keyring.gpg /tmp/caddy-stable.gpg.key
        curl -1sLf https://dl.cloudsmith.io/public/caddy/stable/debian.deb.txt -o /etc/apt/sources.list.d/caddy-stable.list
        chmod o+r /usr/share/keyrings/caddy-stable-archive-keyring.gpg /etc/apt/sources.list.d/caddy-stable.list
        apt-get update -q
        apt-get install -y -q caddy
    fi
    step "Caddy configuration"
    email_option=""
    [ -z "$email" ] || email_option="email $email"
    tls_option=""
    [ "$tls_internal" = 0 ] || tls_option="tls internal"
    install -m 0644 "$vps/Caddyfile" /etc/caddy/Caddyfile
    {
        echo "CADDY_SITE=$site"
        echo "CADDY_EMAIL_OPTION=$email_option"
        echo "CADDY_TLS_OPTION=$tls_option"
        echo "WHEEL_API_UPSTREAM=127.0.0.1:8080"
        echo "WHEEL_WEB_UPSTREAM=127.0.0.1:3000"
        echo "WHEEL_SIGNUP=closed"
    } | write /etc/wheel/caddy.env 0640
    install -d -m 0755 /etc/systemd/system/caddy.service.d
    printf '[Service]\nEnvironmentFile=/etc/wheel/caddy.env\n' | write /etc/systemd/system/caddy.service.d/wheel.conf 0644
    runuser -u caddy -- env HOME=/var/lib/caddy CADDY_SITE="$site" CADDY_EMAIL_OPTION="$email_option" \
        CADDY_TLS_OPTION="$tls_option" WHEEL_API_UPSTREAM=127.0.0.1:8080 WHEEL_WEB_UPSTREAM=127.0.0.1:3000 \
        WHEEL_SIGNUP=closed caddy validate --config /etc/caddy/Caddyfile --adapter caddyfile
    systemctl daemon-reload
    systemctl enable --quiet caddy
    systemctl restart caddy
elif systemctl list-unit-files caddy.service >/dev/null 2>&1; then
    systemctl disable --now --quiet caddy || true
fi

if [ "$firewall" = 1 ]; then
    step "ufw"
    ufw allow 22/tcp >/dev/null
    if [ "$no_proxy" = 0 ]; then
        ufw allow 80/tcp >/dev/null
        ufw allow 443/tcp >/dev/null
        ufw allow 443/udp >/dev/null
    fi
    ufw --force enable
fi

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
