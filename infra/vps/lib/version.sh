# Copyright Morgan Metz
# Licensed under the PolyForm Noncommercial License 1.0.0.
# See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0
# shellcheck shell=sh
#
# Dotted-version comparison, in POSIX shell with no external command.
#
# Why not `sort -V`: it is a GNU coreutils extension. This file is sourced by install.sh and by
# wheel-preflight on an Ubuntu server, where it exists — and by a test that runs in `make check` on
# a developer's Mac, where BSD sort's support for it has varied. A gate that cannot run on the
# machine the developer is using is a gate that stops being run.
#
# Why not a string compare: the exact pair this exists for is `2.1.269` against `2.1.27`. The floor
# for claude's headless OAuth refresh is 2.1.269, and "2.1.269" < "2.1.27" as strings while
# 269 > 27 as numbers. A lexical comparison would silently accept a build two hundred releases too
# old. That pair is in the test table.

# wheel_version_ge <a> <b> — true when version a >= version b.
#
# Compares component by component, numerically, padding the shorter version with zeros so that
# `2.1` and `2.1.0` compare equal rather than unordered. Any non-numeric tail on a component
# (a `-beta` suffix, say) is stripped down to its leading digits, and a component with no leading
# digits counts as 0 — a prerelease therefore compares as its own base version rather than being
# refused outright, which is the forgiving direction for a floor check and the reason the floor is
# also printed whenever it is applied.
wheel_version_ge() {
    _wva="$1"
    _wvb="$2"
    _wvi=1
    while [ "$_wvi" -le 6 ]; do
        _wvx=$(printf '%s' "$_wva" | cut -d. -f"$_wvi")
        _wvy=$(printf '%s' "$_wvb" | cut -d. -f"$_wvi")
        # cut prints the whole string when the field is absent, so absence is detected by counting
        # separators rather than by trusting cut's output.
        [ "$(printf '%s' "$_wva" | tr -cd . | wc -c)" -ge "$((_wvi - 1))" ] || _wvx=0
        [ "$(printf '%s' "$_wvb" | tr -cd . | wc -c)" -ge "$((_wvi - 1))" ] || _wvy=0
        _wvx=$(printf '%s' "$_wvx" | sed 's/[^0-9].*//')
        _wvy=$(printf '%s' "$_wvy" | sed 's/[^0-9].*//')
        [ -n "$_wvx" ] || _wvx=0
        [ -n "$_wvy" ] || _wvy=0
        if [ "$_wvx" -gt "$_wvy" ]; then return 0; fi
        if [ "$_wvx" -lt "$_wvy" ]; then return 1; fi
        _wvi=$((_wvi + 1))
    done
    return 0
}

# wheel_version_of <command> — the first dotted number in `<command> --version`'s output.
#
# Every CLI here decorates its version differently ("2.1.269 (Claude Code)", "codex-cli 0.154.0",
# "v22.11.0"), so the number is extracted rather than the line being parsed per-tool. A tool that
# prints no dotted number at all yields the empty string, and every caller treats that as "could
# not determine" rather than as zero.
wheel_version_of() {
    "$@" --version 2>/dev/null | head -5 |
        grep -oE '[0-9]+\.[0-9]+(\.[0-9]+)?' | head -1
}
