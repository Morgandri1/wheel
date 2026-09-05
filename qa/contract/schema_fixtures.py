#!/usr/bin/env python3
"""Contract test: docs/schema/*.json must accept every valid node fixture and
REJECT every invalid one.

TESTPLAN: NODE-* (all), NODE-schema-roundtrip.

The second half is the half that matters. JSON Schema is permissive by default —
unknown properties are allowed unless `additionalProperties: false`, and a `config`
that belongs to a different node type will sail through unless the schema actually
discriminates on `type`. A schema that accepts all 16 valid fixtures and also accepts
all 26 invalid ones is worthless, and would look green in a naive test. So each
invalid fixture carries the TESTPLAN criterion it exists to prove, and this test
names the criterion when the schema lets it through.
"""
import json, os, sys, glob

SKIP = 77   # exit code meaning "gate could not run"; qa/check.sh reports it as a SKIP, not a pass

ROOT = os.path.normpath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", ".."))
# WHEEL_SCHEMA_DIR lets the selftest point this at a scratch schema. Unset in normal runs.
SCHEMA_DIR = os.environ.get("WHEEL_SCHEMA_DIR") or os.path.join(ROOT, "docs", "schema")
FIXTURES = os.environ.get("WHEEL_FIXTURES_DIR") or os.path.join(ROOT, "qa", "fixtures", "nodes")
VALID = os.path.join(FIXTURES, "valid")
INVALID = os.path.join(FIXTURES, "invalid")

def load(p):
    with open(p) as f:
        return json.load(f)

def find_node_schema():
    """Locate the Node schema without hard-coding SDK's filename."""
    files = sorted(glob.glob(os.path.join(SCHEMA_DIR, "*.json")))
    if not files:
        return None, files
    for pref in ("node.json", "Node.json", "wheel-core.json", "schema.json"):
        p = os.path.join(SCHEMA_DIR, pref)
        if os.path.exists(p):
            return p, files
    for p in files:
        try:
            s = load(p)
        except Exception:
            continue
        blob = json.dumps(s).lower()
        if '"name"' in blob and '"wires"' in blob and '"position"' in blob:
            return p, files
    return None, files

def main():
    try:
        from jsonschema import validators as _jsv
    except ImportError:
        print("jsonschema not installed — run `make bootstrap` (creates qa/.venv)")
        return SKIP

    schema_path, files = find_node_schema()
    if schema_path is None:
        if not files:
            print("docs/schema/*.json not exported yet "
                  "(SDK: cargo run -p wheel-core --bin export-schema)")
            return SKIP
        print("FAIL: found %d schema file(s) in docs/schema but none looks like the Node schema:"
              % len(files))
        for f in files:
            print("   -", os.path.relpath(f, ROOT))
        print("  QA needs to know which file describes a Node — tell me the filename and I'll pin it.")
        return 1

    schema = load(schema_path)
    # docs/schema/*.json declare draft-07; validating them as 2020-12 silently
    # changes how $ref and some keywords behave. Use the declared dialect.
    Vcls = _jsv.validator_for(schema)
    store = {}
    for f in files:  # let $refs across the exported schemas resolve
        try:
            s = load(f)
            if "$id" in s:
                store[s["$id"]] = s
        except Exception:
            pass
    try:
        from jsonschema import RefResolver
        resolver = RefResolver.from_schema(schema, store=store)
        validator = Vcls(schema, resolver=resolver)
    except Exception:
        validator = Vcls(schema)

    print("schema: %s" % os.path.relpath(schema_path, ROOT))
    fails = []

    pending = deferred = 0
    for p in sorted(glob.glob(os.path.join(VALID, "*.json"))):
        name = os.path.basename(p)[:-5]
        doc = load(p)
        why = doc.pop("_pending", None)
        if why:
            pending += 1
            print("  ..   valid/%-26s PENDING — %s" % (name, why))
            continue
        errs = sorted(validator.iter_errors(doc), key=lambda e: list(e.path))
        if errs:
            fails.append("valid/%s REJECTED: %s" % (name, errs[0].message[:140]))
            print("  FAIL valid/%-26s rejected — %s" % (name, errs[0].message[:100]))
        else:
            print("  ok   valid/%s" % name)

    for p in sorted(glob.glob(os.path.join(INVALID, "*.json"))):
        name = os.path.basename(p)[:-5]
        doc = load(p)
        crit = doc.pop("_expect_reject", "?")
        ref = doc.pop("_engine_ref", None)
        if doc.pop("_enforced_by", "schema") == "engine":
            deferred += 1
            print("  ..   invalid/%-26s engine-enforced (%s) — asserted in qa/integration" % (name, ref or crit))
            continue
        if validator.is_valid(doc):
            fails.append("invalid/%s ACCEPTED (violates %s)" % (name, crit))
            print("  FAIL invalid/%-26s accepted, but %s says it must be rejected" % (name, crit))
        else:
            print("  ok   invalid/%-26s rejected (%s)" % (name, crit))

    print()
    if fails:
        print("schema contract: %d FAILED" % len(fails))
        for f in fails:
            print("  -", f)
        return 1
    print("schema contract: every structural fixture behaves as specified "
          "(%d pending, %d deferred to engine validation)" % (pending, deferred))
    return 0

if __name__ == "__main__":
    sys.exit(main())
