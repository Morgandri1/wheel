#!/usr/bin/env python3
"""Selftest for the schema contract test — proves it has teeth BEFORE the real schema lands.

A contract test that passes everything is worse than no contract test. So: run
schema_fixtures.py against a deliberately permissive schema (must FAIL, naming the
criteria it let through) and against a strict one (must PASS).
"""
import json, os, shutil, subprocess, sys, tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.normpath(os.path.join(HERE, "..", ".."))
RUNNER = os.path.join(HERE, "schema_fixtures.py")
PY = os.path.join(ROOT, "qa", ".venv", "bin", "python")
if not os.path.exists(PY):
    PY = sys.executable

PERMISSIVE = {
    "$schema": "https://json-schema.org/draft/2020-12/schema",
    "title": "Node",
    "type": "object",
    "properties": {"id": {}, "name": {}, "type": {}, "position": {}, "wires": {}, "config": {}},
}

NAME_RE = "^[a-z0-9][a-z0-9-_]{0,62}$"
COL = {"type": "object", "additionalProperties": False,
       "required": ["name", "type"],
       "properties": {"name": {"type": "string", "pattern": NAME_RE},
                      "type": {"enum": ["text", "integer", "real", "blob", "json"]}}}

def cfg(props, required, extra=None):
    d = {"type": "object", "additionalProperties": False,
         "required": required, "properties": props}
    if extra:
        d.update(extra)
    return d

def variant(t, config):
    return {"type": "object", "additionalProperties": False,
            "required": ["id", "name", "type", "position", "wires", "config"],
            "properties": {
                "id": {"type": "string"},
                "name": {"type": "string", "pattern": NAME_RE},
                "type": {"const": t},
                "position": {"type": "object", "additionalProperties": False,
                             "required": ["x", "y"],
                             "properties": {"x": {"type": "number"}, "y": {"type": "number"}}},
                "wires": {"type": "array", "items": {
                    "type": "object", "additionalProperties": False,
                    "required": ["to", "type"],
                    "properties": {"to": {"type": "string"},
                                   "type": {"enum": ["read", "write", "send"]}}}},
                "config": config}}

STRICT = {
    "$schema": "https://json-schema.org/draft/2020-12/schema",
    "title": "Node",
    "oneOf": [
        variant("agent", cfg({
            "harness": {"enum": ["claude", "codex"]},
            "model": {"type": ["string", "null"]},
            "system_prompt": {"type": "string"},
            "run_on_startup": {"type": "boolean"},
            "ephemeral_context": {"type": "boolean"}},
            ["harness", "system_prompt", "run_on_startup", "ephemeral_context"])),
        variant("ctx", cfg({"markdown": {"type": "string"}}, ["markdown"])),
        variant("table", cfg({"columns": {"type": "array", "items": COL}}, ["columns"])),
        variant("endpoint", cfg({
            "method": {"enum": ["GET", "POST", "PUT", "DELETE"]},
            "path": {"type": "string", "pattern": "^/(?!.*\\.\\.)[^\\x00]*$"},
            "response_mode": {"enum": ["ack", "script"]}},
            ["method", "path", "response_mode"])),
        variant("script", cfg({
            "language": {"enum": ["python", "ts", "js"]},
            "source": {"type": "string"},
            "timeout_secs": {"type": "number", "exclusiveMinimum": 0}},
            ["language", "source"])),
        variant("mcp", {"allOf": [
            cfg({"transport": {"enum": ["stdio", "http"]},
                 "command": {"type": "string"}, "args": {"type": "array", "items": {"type": "string"}},
                 "url": {"type": "string"}, "env": {"type": "object"}},
                ["transport"]),
            {"oneOf": [
                {"required": ["command"], "not": {"required": ["url"]},
                 "properties": {"transport": {"const": "stdio"}}},
                {"required": ["url"], "not": {"required": ["command"]},
                 "properties": {"transport": {"const": "http"}}}]}]}),
        variant("vault", cfg({"keys": {"type": "array", "items": {"type": "string"}}}, ["keys"])),
        variant("chest", cfg({}, [])),
    ],
}

# The reference STRICT schema below deliberately covers only the ORIGINAL 8 node types
# and their original config shapes. It exists to prove the runner can PASS, not to track
# the contract — chasing every contract addition here would make this fixture a second,
# competing source of truth. So the strict pass runs against a curated fixture subset,
# and anything outside that subset is skipped by prefix.
STRICT_SKIP_PREFIXES = ("tool_", "endpoint_auth_")

def _subset_fixtures(root):
    """Copy fixtures the reference schema is expected to cover into a temp dir."""
    d = tempfile.mkdtemp()
    for kind in ("valid", "invalid"):
        src = os.path.join(root, "qa", "fixtures", "nodes", kind)
        dst = os.path.join(d, kind); os.makedirs(dst)
        if not os.path.isdir(src):
            continue
        for fn in os.listdir(src):
            if fn.endswith(".json") and not fn.startswith(STRICT_SKIP_PREFIXES):
                shutil.copyfile(os.path.join(src, fn), os.path.join(dst, fn))
    return d

def run_against(schema, fixtures_dir=None):
    d = tempfile.mkdtemp()
    with open(os.path.join(d, "node.json"), "w") as f:
        json.dump(schema, f)
    env = dict(os.environ); env["WHEEL_SCHEMA_DIR"] = d
    if fixtures_dir:
        env["WHEEL_FIXTURES_DIR"] = fixtures_dir
    return subprocess.run([PY, RUNNER], capture_output=True, text=True, env=env, timeout=180)

def main():
    fails = []

    # If the runner itself cannot run (no jsonschema), this selftest cannot run either.
    # Exit 77 = SKIP. Treating "could not run" as "failed" is what turned main red.
    probe = run_against(PERMISSIVE)
    if probe.returncode == 77:
        print(probe.stdout.strip() or "runner reported it could not run")
        print("cannot self-test without jsonschema — run `make bootstrap`")
        return 77

    p = probe
    if p.returncode == 0:
        fails.append("permissive schema was ACCEPTED — the contract test has no teeth")
        print("  FAIL permissive schema passed; it should have been caught")
    else:
        leaked = [l for l in p.stdout.splitlines() if "accepted, but" in l]
        print("  ok   permissive schema rejected (%d invalid fixtures correctly flagged)" % len(leaked))
        if len(leaked) < 20:
            fails.append("expected the permissive schema to leak most of the 26 invalid fixtures, "
                         "flagged only %d" % len(leaked))
            print("  FAIL only %d leaks flagged" % len(leaked))

    p = run_against(STRICT, fixtures_dir=_subset_fixtures(ROOT))
    if p.returncode != 0:
        fails.append("strict schema was REJECTED — the fixtures or the runner are wrong")
        print("  FAIL strict schema failed:")
        for l in p.stdout.splitlines():
            if l.strip().startswith("FAIL"):
                print("      " + l.strip())
    else:
        print("  ok   strict schema behaved correctly on the covered fixture subset")

    print()
    if fails:
        print("schema contract selftest: FAILED")
        for f in fails:
            print("  -", f)
        return 1
    print("schema contract selftest: passed")
    return 0

if __name__ == "__main__":
    sys.exit(main())
