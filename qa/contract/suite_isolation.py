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


def check_imports(suite_dir):
    """Names used from wheel_client must be imported -- checked by AST, not by grep.

    I appended ", free_port" to an import line and it landed inside the trailing
    `# noqa` comment, so the module used a name it had never imported and died at import
    time in CI. My own verification was `grep "import.*free_port"`, which matched the
    comment and reported all clear. A grep sees text; only a parser sees an import.
    """
    import ast, os
    watched = {"free_port", "configure_fakes", "pin_image", "run_suite", "Results"}
    bad = []
    for name in sorted(os.listdir(suite_dir)):
        if not name.startswith("test_") or not name.endswith(".py"):
            continue
        path = os.path.join(suite_dir, name)
        try:
            tree = ast.parse(open(path).read())
        except SyntaxError as e:
            bad.append("%s does not parse: %s" % (name, e))
            continue
        imported = set()
        for n in ast.walk(tree):
            if isinstance(n, ast.ImportFrom):
                imported.update(a.asname or a.name for a in n.names)
            elif isinstance(n, ast.Import):
                imported.update((a.asname or a.name).split(".")[0] for a in n.names)
        used = {n.id for n in ast.walk(tree) if isinstance(n, ast.Name)}
        assigned = {t.id for n in ast.walk(tree) if isinstance(n, ast.Assign)
                    for t in n.targets if isinstance(t, ast.Name)}
        funcs = {n.name for n in ast.walk(tree)
                 if isinstance(n, (ast.FunctionDef, ast.AsyncFunctionDef))}
        for w in sorted(watched & used):
            if w not in imported and w not in assigned and w not in funcs:
                bad.append("%s uses %s() without importing it — it will die at import time"
                           % (name, w))
    return bad


def check_free_port(suite_dir):
    """A fixed port makes a leftover container indistinguishable from a broken engine.

    test_engine_messages.py bound a hardcoded 17427. A container of mine from a killed run
    still held it, so the suite's own engine could not bind and it reported "engine never
    became healthy" -- which reads as the ENGINE being broken, not as debris on the host. I
    lost several runs to that before looking at `lsof`.

    Distinct DEFAULTS (checked above) stop two SUITES colliding. They do nothing about the
    same suite running twice, or about anything left behind, which on a six-agent host is
    routine. free_port() prefers the documented default and falls back to any free one.
    """
    import os, re
    bad = []
    for name in sorted(os.listdir(suite_dir)):
        if not name.startswith("test_") or not name.endswith(".py"):
            continue
        src = open(os.path.join(suite_dir, name)).read()
        if re.search(r'^\w*PORT = int\(os\.environ\.get\(', src, re.M) and "free_port(" not in src:
            bad.append(name)
    return ["%s binds a FIXED port — wrap it in free_port() so a leftover container cannot "
            "masquerade as a broken engine" % n for n in bad]


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

    problems.extend(check_free_port(INTEGRATION))
    problems.extend(check_imports(INTEGRATION))

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
