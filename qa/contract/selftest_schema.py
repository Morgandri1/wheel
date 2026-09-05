#!/usr/bin/env python3
"""Selftest for the schema contract runner — proves it can actually FAIL.

A contract test that passes everything is worse than none, so this runs
schema_fixtures.py against a deliberately permissive schema (must FAIL) and a
strict one (must PASS).

It uses its OWN tiny, self-contained fixtures and schema rather than the real
qa/fixtures/nodes tree. Earlier this test mirrored the real node contract, which
meant every contract change (endpoint auth, the 9th `tool` type) broke it — the
selftest became a second, always-stale implementation of the schema. Its job is
only to prove the RUNNER discriminates, so it needs the smallest input that can
show that, and nothing that tracks the contract.
"""
import json, os, subprocess, sys, tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.normpath(os.path.join(HERE, "..", ".."))
RUNNER = os.path.join(HERE, "schema_fixtures.py")
PY = os.path.join(ROOT, "qa", ".venv", "bin", "python")
if not os.path.exists(PY):
    PY = sys.executable

PERMISSIVE = {"$schema": "https://json-schema.org/draft/2020-12/schema",
              "title": "Node", "type": "object"}

STRICT = {
    "$schema": "https://json-schema.org/draft/2020-12/schema",
    "title": "Node",
    "type": "object",
    "additionalProperties": False,
    "required": ["name", "type", "config"],
    "properties": {
        "name": {"type": "string", "pattern": "^[a-z0-9][a-z0-9-_]{0,62}$"},
        "type": {"enum": ["ctx"]},
        "config": {"type": "object", "additionalProperties": False,
                   "required": ["markdown"],
                   "properties": {"markdown": {"type": "string"}}},
    },
}

VALID = {
    "plain":    {"name": "notes", "type": "ctx", "config": {"markdown": "# hi"}},
    "name_min": {"name": "a", "type": "ctx", "config": {"markdown": ""}},
}
INVALID = {
    "name_uppercase":     {"name": "Notes", "type": "ctx", "config": {"markdown": "x"}},
    "name_leading_dash":  {"name": "-bad", "type": "ctx", "config": {"markdown": "x"}},
    "type_unknown":       {"name": "n", "type": "wormhole", "config": {"markdown": "x"}},
    "config_unknown_key": {"name": "n", "type": "ctx", "config": {"markdown": "x", "extra": 1}},
    "config_missing":     {"name": "n", "type": "ctx", "config": {}},
    "unknown_top_key":    {"name": "n", "type": "ctx", "config": {"markdown": "x"}, "bogus": 1},
}

def build_fixtures():
    d = tempfile.mkdtemp()
    for sub, items in (("valid", VALID), ("invalid", INVALID)):
        os.makedirs(os.path.join(d, sub))
        for name, doc in items.items():
            doc = dict(doc)
            if sub == "invalid":
                doc["_expect_reject"] = "SELFTEST-%s" % name
                doc["_enforced_by"] = "schema"
            with open(os.path.join(d, sub, name + ".json"), "w") as f:
                json.dump(doc, f)
    return d

def run_against(schema, fixtures):
    sd = tempfile.mkdtemp()
    with open(os.path.join(sd, "node.json"), "w") as f:
        json.dump(schema, f)
    env = dict(os.environ)
    env["WHEEL_SCHEMA_DIR"] = sd
    env["WHEEL_FIXTURES_DIR"] = fixtures
    return subprocess.run([PY, RUNNER], capture_output=True, text=True, env=env, timeout=120)

def main():
    fixtures = build_fixtures()
    fails = []

    probe = run_against(PERMISSIVE, fixtures)
    if probe.returncode == 77:
        print(probe.stdout.strip() or "runner reported it could not run")
        print("cannot self-test without jsonschema — run `make bootstrap`")
        return 77

    if probe.returncode == 0:
        fails.append("permissive schema was ACCEPTED — the contract runner has no teeth")
        print("  FAIL permissive schema passed; every invalid fixture should have been flagged")
    else:
        leaked = [l for l in probe.stdout.splitlines() if "accepted, but" in l]
        if len(leaked) == len(INVALID):
            print("  ok   permissive schema rejected (all %d invalid fixtures flagged)" % len(leaked))
        else:
            fails.append("permissive schema leaked %d/%d invalid fixtures; the runner missed some"
                         % (len(leaked), len(INVALID)))
            print("  FAIL only %d of %d leaks flagged" % (len(leaked), len(INVALID)))

    p = run_against(STRICT, fixtures)
    if p.returncode != 0:
        fails.append("strict schema was REJECTED — the runner reports false failures")
        print("  FAIL strict schema failed:")
        for l in p.stdout.splitlines():
            if "FAIL" in l:
                print("      " + l.strip())
    else:
        print("  ok   strict schema accepted all %d valid and rejected all %d invalid"
              % (len(VALID), len(INVALID)))

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
