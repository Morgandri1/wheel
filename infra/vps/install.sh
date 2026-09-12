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
#   --rollback         swap /opt/wheel/bin/*.prev back and restart. No build, no clone, no network.
#   --allow-downgrade  install a --ref the running binary is already ahead of (see below)
#
# UPGRADE, ROLLBACK, AND THE OTHER LANE. This script owns the OPERATOR-INITIATED lifecycle: first
# install, a deliberate upgrade to a named --ref, and rolling that back. `sdk/auto-update` owns the
# DAEMON-initiated one: wheeld noticing main moved, checking CI, draining, swapping the binaries
# itself. They share a filesystem contract and nothing else, so three rules keep them from fighting:
#
#   1. This script NEVER deletes /opt/wheel/bin/*.prev. That is the other lane's rollback artefact.
#   2. It uses the same swap shape the daemon uses -- write .new, hard-link the current to .prev,
#      rename(2) over -- so there is one rollback artefact with one meaning, whoever wrote it.
#   3. It refuses to move the binaries BACKWARDS by default. If the installed build's commit is a
#      descendant of --ref, a self-applied update is about to be clobbered; --allow-downgrade says
#      you mean it.
#
# Neither lane writes WHEEL_AUTO_UPDATE. That is yours, in /etc/wheel/wheeld.local.env, off unless
# you set it.
#
# There is no plain-HTTP-publicly mode, on purpose, matching compose.yml. An earlier version had
# one (--public-host): Caddy would adapt `:80` with no host matcher, so the HSTS route's
# `protocol https` match never fired, and WHEEL_PUBLIC_ORIGIN=http://<host> meant the session
# cookie lost both Secure and the __Host- prefix. A password does not get a second chance once it
# has crossed the network once in the clear — see README.md and preflight.sh's compose-side
# reasoning, which applies here unchanged.
#
# Layout. src and bin are the auto-update hook points (WHEEL_UPDATE_REPO, WHEEL_UPDATE_BIN_DIR):
#   /opt/wheel/src   git checkout          /opt/wheel/bin    wheeld, wheel, and *.prev
#   /opt/wheel/web   web app server        /opt/wheel/rust   shared Rust toolchain
#   /opt/wheel/libexec  preflight, ready, doctor, the signup gate (root-owned: /opt/wheel/src is
#                       wheel-writable under --updatable, and root must not exec from there)
#   /var/lib/wheel   data, 0700, `wheel`   /var/cache/wheel  build caches, `wheel-build`
#   /etc/wheel       settings              units: wheeld, wheel-web, wheel-signup-gate, caddy

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
rollback=0
allow_downgrade=0

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
        --rollback) rollback=1; shift ;;
        --allow-downgrade) allow_downgrade=1; shift ;;
        -h | --help) sed -n '6,52p' "$0"; exit 0 ;;
        *) die "unknown argument $1 (see --help)" ;;
    esac
done

[ "$(id -u)" = 0 ] || die "run as root (sudo)"
# shellcheck source=/dev/null
. /etc/os-release
[ "${ID:-}" = ubuntu ] && [ "${VERSION_ID:-}" = 24.04 ] || die "this installer targets Ubuntu 24.04, not ${PRETTY_NAME:-this system}"

# Swaps /opt/wheel/bin/{wheeld,wheel} with their .prev generation. Hard-link, then rename(2): both
# names exist at every instant and the swap is atomic per file, so a crash mid-rollback leaves a
# runnable binary rather than a missing one. The OLD current becomes the new .prev, so a rollback
# is itself rollbackable -- you can toggle between two generations without a rebuild.
swap_to_prev() {
    local missing=""
    for bin in wheeld wheel; do
        [ -e "/opt/wheel/bin/$bin.prev" ] || missing="$missing $bin.prev"
    done
    [ -z "$missing" ] || die "no previous generation to roll back to (missing:$missing). /opt/wheel/bin/*.prev is written by an upgrade -- a first install has nothing behind it."
    for bin in wheeld wheel; do
        ln -f "/opt/wheel/bin/$bin" "/opt/wheel/bin/.$bin.swap"
        ln -f "/opt/wheel/bin/$bin.prev" "/opt/wheel/bin/.$bin.new"
        mv -f "/opt/wheel/bin/.$bin.new" "/opt/wheel/bin/$bin"
        mv -f "/opt/wheel/bin/.$bin.swap" "/opt/wheel/bin/$bin.prev"
    done
}

# --rollback is deliberately a SEPARATE, SHORT PATH: no clone, no fetch, no build, no package
# manager, no network. The moment you need it is the moment an upgrade went wrong, and the last
# thing that should stand between you and a serving box is apt.
if [ "$rollback" = 1 ]; then
    [ -z "$domain" ] && [ "$no_proxy" = 0 ] || die "--rollback takes no mode: it restores the previous binaries and restarts, and changes no setting"
    step "rollback: $(/opt/wheel/bin/wheeld --version 2>/dev/null || echo 'current unknown') -> $(/opt/wheel/bin/wheeld.prev --version 2>/dev/null || echo 'previous unknown')"
    if [ "$dry_run" = 1 ]; then
        echo "   (dry-run) would swap /opt/wheel/bin/{wheeld,wheel} with their .prev and restart wheeld"
        exit 0
    fi
    swap_to_prev
    systemctl restart wheeld
    /opt/wheel/bin/wheeld --version
    echo "Rolled back. The generation you rolled back FROM is now /opt/wheel/bin/wheeld.prev, so this is reversible: run --rollback again."
    exit 0
fi

modes=0
[ -z "$domain" ] || modes=$((modes + 1))
[ "$no_proxy" = 0 ] || modes=$((modes + 1))
[ "$modes" = 1 ] || die "choose exactly one of --domain and --no-proxy"
[ "$tls_internal" = 0 ] || [ -n "$domain" ] || die "--tls-internal needs --domain"

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
#
# iproute2 is a real prerequisite, not a convenience: libexec/wheeld-ready measures the listening
# socket with `ss` to prove the loopback promise, and a check that cannot run must not look like a
# check that passed. sqlite3 is what backup.sh and migrate-from-docker.sh use to say "the database
# that arrived is a database", rather than "a file of the right size arrived".
run apt-get install -y -q --no-install-recommends \
    ca-certificates curl git gnupg build-essential pkg-config libssl-dev make python3 python3-venv procps \
    iproute2 sqlite3

# THE TOOLCHAIN DOCKER USED TO BUNDLE. The image shipped Node, git and the CLIs, so nothing on the
# host had to be right. Native means the host supplies them, so they are pinned in one file and
# re-checked by wheel-preflight on every service start -- not just here, once, at install time.
here="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=/dev/null
. "$here/toolchain.env"
# shellcheck source=/dev/null
. "$here/lib/version.sh"

if ! node_version="$(node --version 2>/dev/null)" || [ "${node_version%%.*}" != "v$WHEEL_NODE_MAJOR" ]; then
    step "Node.js $WHEEL_NODE_MAJOR from NodeSource"
    if [ "$dry_run" = 1 ]; then
        echo "   (dry-run) would install Node.js $WHEEL_NODE_MAJOR from NodeSource"
    else
        curl -fsSL "https://deb.nodesource.com/setup_$WHEEL_NODE_MAJOR.x" -o "$install_tmp/nodesource_setup.sh"
        bash "$install_tmp/nodesource_setup.sh"
        apt-get install -y -q nodejs
    fi
fi
step "pinned CLIs: pnpm $WHEEL_PNPM_VERSION, claude $WHEEL_CLAUDE_VERSION, codex $WHEEL_CODEX_VERSION"
run npm install -g --no-fund --no-audit --loglevel=error \
    "pnpm@$WHEEL_PNPM_VERSION" \
    "@anthropic-ai/claude-code@$WHEEL_CLAUDE_VERSION" \
    "@openai/codex@$WHEEL_CODEX_VERSION"

# VERIFIED AGAINST WHAT THE BINARY PRINTS, never against what npm was asked to install. An npm
# cache, a pre-existing global install, or an upgrade that half-failed all produce a box where the
# pin says one thing and the binary on PATH is another -- and the symptom is not a failed install,
# it is an OAuth refresh that stops working a week later on a box nobody is watching. PR #64's
# headless refresh is the thing that needs the floor.
if [ "$dry_run" = 0 ]; then
    # `|| true` is load-bearing, not defensive noise. wheel_version_of ends in a grep, this script
    # sets pipefail, and a claude that prints an unparseable banner therefore makes the whole
    # command substitution exit 1 -- so `set -e` would abort here with NO output, in exactly the
    # case the comment above says is the one worth catching. The empty-check below is the
    # diagnostic; it has to be reachable.
    claude_installed="$(wheel_version_of claude || true)"
    [ -n "$claude_installed" ] || die "claude is installed at $(command -v claude || echo 'nowhere on PATH') but printed no parseable version, so its fitness for headless OAuth refresh cannot be confirmed"
    wheel_version_ge "$claude_installed" "$WHEEL_CLAUDE_MIN" ||
        die "claude reports $claude_installed, below the $WHEEL_CLAUDE_MIN floor that PR #64's headless OAuth refresh requires. npm was asked for $WHEEL_CLAUDE_VERSION, so something else on this box is shadowing it: check 'command -v claude' and 'npm ls -g --depth=0'."
    echo "    claude $claude_installed (floor $WHEEL_CLAUDE_MIN) ok"
fi

step "users and directories"
id wheel >/dev/null 2>&1 || run useradd --system --home-dir /var/lib/wheel --no-create-home --shell /usr/sbin/nologin wheel
id wheel-build >/dev/null 2>&1 || run useradd --system --home-dir /var/cache/wheel --no-create-home --shell /usr/sbin/nologin wheel-build
run install -d -m 0700 -o wheel -g wheel /var/lib/wheel
run install -d -m 0755 -o wheel-build -g wheel-build /var/cache/wheel
run install -d -m 0755 -o root -g root /opt/wheel /opt/wheel/bin /opt/wheel/rust /opt/wheel/libexec /etc/wheel

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

# WOULD THIS UNDO A SELF-APPLIED UPDATE? /opt/wheel/bin is also sdk/auto-update's write target: a
# wheeld running with WHEEL_AUTO_UPDATE=auto rebuilds itself from /opt/wheel/src and swaps the
# binaries in place. Re-running this script with the --ref you first installed would then walk the
# box silently backwards -- no error, no warning, just an older binary and a wheeld that has to
# re-apply an update it already made. So: if what is installed is a DESCENDANT of the target, say
# so and stop.
#
# Only a refusal, never a correction. Moving backwards is sometimes exactly what you want (that is
# what a bad update feels like), and --allow-downgrade is how you say so. Ancestry is the test
# rather than timestamps, because a rebuild of the same commit has a newer mtime and is not a
# downgrade at all.
if [ "$dry_run" = 0 ] && [ -x /opt/wheel/bin/wheeld ]; then
    installed_sha="$(/opt/wheel/bin/wheeld --version 2>/dev/null | sed -n 's/.*(\([0-9a-f]\{7,40\}\)).*/\1/p')"
    if [ -n "$installed_sha" ] && git_src cat-file -e "$installed_sha^{commit}" 2>/dev/null &&
        [ "$installed_sha" != "$commit" ] &&
        git_src merge-base --is-ancestor "$commit" "$installed_sha" 2>/dev/null; then
        [ "$allow_downgrade" = 1 ] ||
            die "the installed binary is $installed_sha, which is AHEAD of --ref $ref ($commit). Installing would move this box backwards and clobber an update it applied itself (WHEEL_AUTO_UPDATE). Use --ref with something at or past $installed_sha, or --allow-downgrade if going backwards is the point."
        echo "    WARNING: installed $installed_sha is ahead of the target; --allow-downgrade says you mean it"
    fi
fi
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
    # The same swap shape sdk/auto-update uses, so there is ONE rollback artefact with ONE meaning
    # whoever wrote it: write .new, hard-link the current generation to .prev, rename(2) over.
    # rename is atomic, so the name /opt/wheel/bin/wheeld always resolves to a complete binary --
    # which matters because systemd may be restarting the unit while this runs.
    #
    # This is a behaviour change, and it is a bug fix: the previous version did `mv -f` over the
    # top and kept no .prev at all, so an operator upgrade DESTROYED the daemon's rollback point.
    # Nothing here ever deletes a .prev.
    for bin in wheeld wheel; do
        install -m 0755 "/var/cache/wheel/target/release/$bin" "/opt/wheel/bin/.$bin.new"
        [ ! -e "/opt/wheel/bin/$bin" ] || ln -f "/opt/wheel/bin/$bin" "/opt/wheel/bin/$bin.prev"
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
    # NO web.prev IS KEPT, so --rollback and the auto-rollback below revert the BINARIES ONLY.
    # That is deliberate rather than an oversight: a board build is ~100 MB against a ~30 MB
    # binary, and the board talks to wheeld over a versioned HTTP API rather than a linked
    # interface, so a newer board against an older wheeld is the ordinary state of affairs during
    # any rolling deploy. If that ever stops being true, this is the line to change. Said out loud
    # because "rolled back" reads like "everything reverted", and here it does not.
    rm -rf /opt/wheel/web.prev
fi

step "settings in /etc/wheel (yours go in *.local.env, which this never touches)"
{
    # The single source of truth for where wheeld listens. wheeld.service interpolates it into
    # --bind, and wheel-preflight and wheeld-ready both read it, so an operator who overrides it in
    # wheeld.local.env gets a daemon, a preflight and a readiness probe that all agree. Loopback:
    # nothing in this kit publishes wheeld to the network (README.md section 1).
    echo "BIND_ADDR=127.0.0.1:8080"
    echo "PUBLIC_BASE_URL=$api_base"
    [ -z "$allowed_hosts" ] || echo "WHEEL_ALLOWED_HOSTS=$allowed_hosts"
    # TLS mode only, and the one guarantee native cannot reproduce (proposal §3). Caddy dials
    # 127.0.0.1:8080, and so can any agent -- agents run as a normal uid on this same host and
    # there is nothing at the socket that tells the two apart. So trusting 127.0.0.1 means wheeld
    # believes X-Forwarded-For from an agent exactly as it believes it from Caddy. Docker's version
    # trusted one container IP on an internal network that no agent could send from.
    #
    # ::1 is dropped: WHEEL_API_UPSTREAM is 127.0.0.1:8080 and nothing ever dialled the v6
    # loopback, so trusting it only widened the set for no reason.
    #
    # In tunnel mode this line is ABSENT, and that is not an oversight -- with no trusted proxy
    # configured wheeld believes no forwarded header from anybody, so the weakness does not exist
    # in the default mode at all. Follow-up F2 is the real fix.
    [ "$no_proxy" = 1 ] || echo "WHEEL_TRUSTED_PROXIES=127.0.0.1/32"
    echo "WHEEL_SIGNUP=closed"
    echo "RUST_LOG=info"
    # There is no tty under systemd, so git's credential helper has nothing to prompt on. Without
    # this, an agent cloning a private or misspelled repo gets "could not read Username for
    # https://github.com: No such device or address" -- which reads like a broken sandbox and sent
    # this lane's own hardening probe chasing the wrong thing for a while. With it, the error names
    # the actual problem. Measured, not guessed: infra/vps/rehearsal/native/harden-probe.sh pins
    # both halves.
    echo "GIT_TERMINAL_PROMPT=0"
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

step "libexec: the scripts the units call"
# Installed OUT of the source checkout, into a root-owned directory, because /opt/wheel/src becomes
# wheel-writable under --updatable and these run as ExecStartPre (root). A checkout an agent can
# write must never be the thing systemd executes as root.
run install -m 0755 "$vps/libexec/wheel-preflight" /opt/wheel/libexec/wheel-preflight
run install -m 0755 "$vps/libexec/wheeld-ready" /opt/wheel/libexec/wheeld-ready
run install -m 0755 "$vps/libexec/wheel-doctor" /opt/wheel/libexec/wheel-doctor
run install -m 0755 "$vps/verify-signup-gate.sh" /opt/wheel/libexec/verify-signup-gate.sh
run install -m 0644 "$vps/lib/version.sh" /opt/wheel/libexec/version.sh
run install -m 0644 "$vps/toolchain.env" /etc/wheel/toolchain.env
run ln -sfn /opt/wheel/libexec/wheel-doctor /usr/local/bin/wheel-doctor

step "systemd units"
run install -m 0644 "$vps/systemd/wheeld.service" /etc/systemd/system/wheeld.service
run install -m 0644 "$vps/systemd/wheel-web.service" /etc/systemd/system/wheel-web.service
run install -m 0644 "$vps/systemd/wheel-signup-gate.service" /etc/systemd/system/wheel-signup-gate.service
# Resource limits are a drop-in, not part of the unit, so an operator on a bigger box overrides
# them in 90-local.conf without editing a file this script overwrites. Drop-ins apply in lexical
# order, so 90- wins over 10-.
run install -m 0644 "$vps/systemd/wheeld.service.d/10-resources.conf" /etc/systemd/system/wheeld.service.d/10-resources.conf
run systemctl daemon-reload
run systemctl enable --quiet wheeld wheel-web wheel-signup-gate

# THE UPGRADE MOMENT. Everything above built; nothing above touched the running daemon. This
# restart is the only downtime, and KillMode=mixed plus TimeoutStopSec=35 is what makes it a drain
# (~28s for turns in flight, then every agent's process group) rather than a kill.
#
# What is new is what happens when the new build does not come up: the previous generation goes
# back and the daemon is restarted again. A failed upgrade should leave a SERVING box and a red
# exit code, not a down box and a red exit code.
if [ "$dry_run" = 1 ]; then
    echo "   (dry-run) would restart wheeld, wait for it to serve, and roll back to the previous generation if it does not"
else
    step "restarting wheeld (drains in-flight turns, ~28s)"
    restart_ok=1
    # ExecStartPost=wheeld-ready already polls /healthz and measures the listening socket, so
    # `systemctl restart` does not return until wheeld is genuinely serving -- or fails. There is
    # deliberately no second health loop here: two implementations of "is it up" drift, and the
    # one in the unit is the one that runs on every boot rather than only on an install.
    systemctl restart wheeld || restart_ok=0
    if [ "$restart_ok" = 0 ]; then
        echo "install: wheeld did not come up at $commit." >&2
        journalctl -u wheeld -n 30 --no-pager >&2 || true
        # BOTH, because that is what swap_to_prev requires. Testing only wheeld.prev could take
        # the rollback branch and then die inside swap_to_prev on the missing wheel.prev, leaving
        # the box DOWN with a message contradicting the test that got us there.
        if [ -e /opt/wheel/bin/wheeld.prev ] && [ -e /opt/wheel/bin/wheel.prev ]; then
            step "ROLLING BACK to the previous generation"
            swap_to_prev
            if systemctl restart wheeld; then
                die "wheeld failed to start at $commit and has been ROLLED BACK. It is serving again on the previous generation ($(/opt/wheel/bin/wheeld --version)). The failed build is now /opt/wheel/bin/wheeld.prev; nothing was lost. Fix the build and re-run, or keep this generation."
            fi
            die "wheeld failed to start at $commit, AND the rollback to the previous generation also failed to start. This box is down. journalctl -u wheeld -n 50"
        fi
        die "wheeld failed to start at $commit and there is no previous generation to roll back to (this is a first install). journalctl -u wheeld -n 50"
    fi

    # The gate is now a UNIT (wheel-signup-gate.service), which wheel-web Requires= and After=, so
    # it re-runs on every boot instead of only here. Running it explicitly at install time is still
    # worth doing: it is the difference between an install that fails loudly now and one that
    # succeeds while leaving a box whose NEXT reboot will not bring the board up.
    step "verifying wheeld enforces its own signup gate"
    systemctl restart wheel-signup-gate ||
        die "$(journalctl -u wheel-signup-gate -n 10 --no-pager 2>/dev/null | tail -5)

wheeld does not enforce its own signup gate, so wheel-web and Caddy were not started in front of it. A config value is not a control until the binary that reads it is proven to enforce it."
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
    # Caddy is ordered behind the signup gate exactly as wheel-web is, and for the same reason:
    # compose's `depends_on: { verify-signup-gate: service_completed_successfully }` applied to
    # BOTH, so the public-facing listener never comes up in front of a wheeld that has not been
    # proven to enforce its own gate. Requires= propagates the failure; After= orders it.
    {
        printf '[Unit]\nRequires=wheel-signup-gate.service\nAfter=wheel-signup-gate.service\n\n'
        printf '[Service]\nEnvironmentFile=/etc/wheel/caddy.env\n'
    } | write /etc/systemd/system/caddy.service.d/wheel.conf 0644
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
  data         /var/lib/wheel  (master.key and operator-token live here)
  operator     sudo cat /var/lib/wheel/operator-token   (then add your account: see README.md)
  logs         journalctl -fu wheeld -u wheel-web -u wheel-signup-gate$([ "$no_proxy" = 0 ] && echo " -u caddy")
  diagnose     sudo wheel-doctor            running vs serving vs actually authenticating
  agents       sudo wheel-doctor agents     the live process tree, per project
  rollback     sudo $0 --rollback           back to $(/opt/wheel/bin/wheeld.prev --version 2>/dev/null || echo 'nothing yet — this is the first generation')

BACK UP /var/lib/wheel. Losing master.key loses every vault secret on this board, permanently:
  sudo $vps/backup.sh --to /var/backups/wheel     (and copy it OFF this machine, encrypted)
EOF
if [ "$no_proxy" = 0 ]; then
    echo "
TLS mode trusts X-Forwarded-For from 127.0.0.1, which on this box means every local process --
agents included, because they run as the \`wheel\` user on this same host. Caddy's forwarded
headers and an agent's forged ones are indistinguishable at the socket. Bounded (rate-limit evasion
and false client addresses in logs, not authentication), unavoidable natively today, and the reason
tunnel mode sets no trusted proxy at all. docs/proposals/wheeld-native-production.md §3."
fi
if [ "$firewall" = 0 ]; then
    echo "  firewall     not touched. Suggested: ufw allow 22/tcp; ufw allow 80/tcp; ufw allow 443/tcp; ufw enable"
fi
