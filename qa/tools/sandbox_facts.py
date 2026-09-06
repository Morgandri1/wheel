#!/usr/bin/env python3
"""What is ACTUALLY true inside a running sandbox: disk, memory, toolchain.

Every number here is read from inside the container, never from a control plane. Three of
today's wrong turns came from the gap between those two: two resource claims taken from an
orchestrator rather than the process, and one test of mine that read an unwritable directory
as a full disk and nearly became a defect report. On a hostile volume "is it full?" has to be
measured, and the measurement takes a second.

    python3 qa/tools/sandbox_facts.py                  # every wheel container
    python3 qa/tools/sandbox_facts.py <container>      # one
    python3 qa/tools/sandbox_facts.py --json

WHAT IT REPORTS AND WHY EACH ONE EARNED ITS PLACE:
  df /data      free bytes AND whether the directory is WRITABLE BY THE RUNNING UID. A
                root-owned mount looks empty and refuses every write, which is the exact
                shape that fooled me — "0% used" is not the same as "you can write here".
  memory        cgroup v2 memory.current / memory.max, because a container OOM-killed at a
                limit the host never noticed is invisible from outside.
  toolchain     RUSTUP_HOME and CARGO_HOME as the CHILD sees them, with mode and owner. A
                path that exists for root and not for the agent is the difference between a
                working build and "no default toolchain configured".
"""
import json
import subprocess
import sys


def sh(*a):
    return subprocess.run(a, capture_output=True, text=True)


def containers():
    out = sh("docker", "ps", "--format", "{{.Names}}").stdout.split()
    return [n for n in out if "wheel" in n or n.startswith("qa-")]


PROBE = r"""
printf 'uid=%s\n' "$(id -u)"
printf 'user=%s\n' "$(id -un 2>/dev/null || echo '?')"
for d in /data /opt/rust; do
  if [ -d "$d" ]; then
    set -- $(df -k "$d" 2>/dev/null | tail -1)
    # One field per LINE: the parser splits on the first '=', so three values on one
    # line collapse into a single unusable key. This printed nothing at all the first
    # time, which is the quiet kind of wrong -- no error, just no facts.
    printf 'df_size_k:%s=%s\n' "$d" "$2"
    printf 'df_used_k:%s=%s\n' "$d" "$3"
    printf 'df_avail_k:%s=%s\n' "$d" "$4"
    if [ -w "$d" ]; then printf 'writable:%s=yes\n' "$d"; else printf 'writable:%s=NO\n' "$d"; fi
    printf 'mode:%s=%s\n' "$d" "$(stat -c '%a %U:%G' "$d" 2>/dev/null || echo '?')"
  else
    printf 'df_absent:%s=yes\n' "$d"
  fi
done
printf 'mem_current=%s\n' "$(cat /sys/fs/cgroup/memory.current 2>/dev/null || echo '?')"
printf 'mem_max=%s\n' "$(cat /sys/fs/cgroup/memory.max 2>/dev/null || echo '?')"
printf 'rustup_home=%s\n' "${RUSTUP_HOME:-<unset>}"
printf 'cargo_home=%s\n' "${CARGO_HOME:-<unset>}"
printf 'cargo_bin=%s\n' "$(command -v cargo || echo '<not on PATH>')"
printf 'cargo_says=%s\n' "$(cargo --version 2>&1 | head -1)"
"""


def facts(name):
    p = sh("docker", "exec", name, "sh", "-c", PROBE)
    out = {"container": name}
    if p.returncode != 0:
        out["error"] = (p.stderr or "could not exec").strip()[:200]
        return out
    for line in p.stdout.splitlines():
        if "=" in line:
            k, _, v = line.partition("=")
            out[k.strip()] = v.strip()
    return out


def human(f):
    if "error" in f:
        return "  %-28s ERROR %s" % (f["container"], f["error"])
    rows = ["  %s (uid %s / %s)" % (f["container"], f.get("uid", "?"), f.get("user", "?"))]
    for d in ("/data", "/opt/rust"):
        avail = f.get("df_avail_k:%s" % d)
        if avail:
            pct = ""
            try:
                size = int(f.get("df_size_k:%s" % d, 0))
                used = int(f.get("df_used_k:%s" % d, 0))
                if size:
                    pct = " (%d%% used)" % round(100.0 * used / size)
            except ValueError:
                pass
            rows.append("      %-10s %8s KiB free%s   writable=%s  %s"
                        % (d, avail, pct, f.get("writable:%s" % d, "?"),
                           f.get("mode:%s" % d, "")))
    rows.append("      memory     current=%s max=%s"
                % (f.get("mem_current", "?"), f.get("mem_max", "?")))
    rows.append("      toolchain  RUSTUP_HOME=%s CARGO_HOME=%s"
                % (f.get("rustup_home", "?"), f.get("cargo_home", "?")))
    rows.append("      cargo      %s" % f.get("cargo_says", "?"))
    return "\n".join(rows)


def main(argv):
    as_json = "--json" in argv
    names = [a for a in argv[1:] if not a.startswith("--")] or containers()
    if not names:
        print("no running wheel containers")
        return 0
    got = [facts(n) for n in names]
    if as_json:
        print(json.dumps(got, indent=2))
    else:
        print("sandbox facts — measured inside each container, not asked of a control plane")
        for f in got:
            print(human(f))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
