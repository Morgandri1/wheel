#!/usr/bin/env python3
"""Every integration suite must own its port, its port's env var, and its container name.

`run.sh` runs the suites serially, so sharing a port works right up until it doesn't:
a suite whose container outlives its `finally` (killed run, docker hiccup), or any attempt
to run two suites at once, turns a shared port into a bind failure or — worse — into one
suite talking to another suite's engine and reporting confident nonsense about it.

Three suites shared 17413 and two shared 17414 before this gate existed, and two different
suites read WHEEL_ENGINE_PORT with DIFFERENT defaults, so setting that variable to relocate
one of them silently collided the other. None of it had failed yet. That is the point:
this is a latent flake, and a latent flake is cheaper to forbid than to debug at 2am.
"""
import os, re, sys

HERE = os.path.dirname(os.path.abspath(__file__))
INTEGRATION = os.path.join(os.path.dirname(HERE), "integration")

PORT_RE = re.compile(r'os\.environ\.get\(\s*"([A-Z0-9_]+)"\s*,\s*"(\d+)"\s*\)')
NAME_RE = re.compile(r'^NAME\s*=\s*"([^"]+)"', re.M)


def main():
    if not os.path.isdir(INTEGRATION):
        print("no qa/integration directory")
        return 1

    ports, envs, names, problems = {}, {}, {}, []
    for fn in sorted(os.listdir(INTEGRATION)):
        if not (fn.startswith("test_") and fn.endswith(".py")):
            continue
        src = open(os.path.join(INTEGRATION, fn)).read()
        for env, port in PORT_RE.findall(src):
            if not (1024 <= int(port) <= 65535):
                continue
            ports.setdefault(port, []).append(fn)
            envs.setdefault(env, set()).add(port)
        for name in NAME_RE.findall(src):
            # A name built from a uuid is unique by construction.
            names.setdefault(name, []).append(fn)

    for port, files in sorted(ports.items()):
        if len(set(files)) > 1:
            problems.append("port %s is the default in %s" % (port, ", ".join(sorted(set(files)))))
    for env, seen in sorted(envs.items()):
        if len(seen) > 1:
            problems.append("%s has %d different defaults (%s) — setting it moves one suite "
                            "onto another" % (env, len(seen), ", ".join(sorted(seen))))
    for name, files in sorted(names.items()):
        if len(set(files)) > 1:
            problems.append("container name %r is used by %s"
                            % (name, ", ".join(sorted(set(files)))))

    if problems:
        print("suite isolation violated:")
        for p in problems:
            print("  - " + p)
        print("\nGive each suite its own default port, its own env var, and its own "
              "container name.")
        return 1

    print("suite isolation ok: %d ports, %d env vars, %d container names, all distinct"
          % (len(ports), len(envs), len(names)))
    return 0


if __name__ == "__main__":
    sys.exit(main())
