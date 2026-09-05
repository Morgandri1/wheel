#!/usr/bin/env python3
"""Contract test: wheel-core's exported wire matrix must equal the ARCHITECTURE.md matrix.

TESTPLAN: WM-export-conformance.

QA derives its matrix INDEPENDENTLY from the §3 prose (qa/tools/gen_wire_matrix.py).
This test compares that against SDK's generated docs/schema/wire-matrix.json.

That independence is the entire point. Web originally hand-transcribed the table into
their own test and it went stale — two copies that agree with each other catch nothing.
Their fix (derive from the generated export) is right FOR A CLIENT: the export is what
the UI must conform to. But QA cannot derive from the export, because then the export
would be checked against itself and no divergence from the written contract could ever
be detected. QA's copy is not a duplicate of the implementation; it is a transcription
of the SPEC, and this test is the diff between spec and implementation.

That is what found BUG-004: two rows present in the prose and missing from the export.
"""
import json, os, sys

SKIP = 77
ROOT = os.path.normpath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", ".."))
EXPORT = os.path.join(ROOT, "docs", "schema", "wire-matrix.json")
QA_MATRIX = os.path.join(ROOT, "qa", "fixtures", "wire_matrix.json")
KNOWN_GAPS = {
    ("endpoint", "vault", "read"): "BUG-004",
    ("script", "tool", "read"): "BUG-004",
}

def main():
    if not os.path.exists(EXPORT):
        print("docs/schema/wire-matrix.json not exported yet (SDK)")
        return SKIP
    if not os.path.exists(QA_MATRIX):
        print("qa/fixtures/wire_matrix.json missing — run qa/tools/gen_wire_matrix.py --write")
        return 1

    sdk = {(a["from"], a["to"], a["type"]) for a in json.load(open(EXPORT)).get("allowed", [])}
    qa = {(c["from"], c["to"], c["type"])
          for c in json.load(open(QA_MATRIX))["cells"] if c["expect"] == "allow"}

    missing = sorted(qa - sdk)   # in the contract, absent from the implementation
    extra = sorted(sdk - qa)     # in the implementation, absent from the contract

    fails, gaps = [], []
    for t in missing:
        bug = KNOWN_GAPS.get(t)
        if bug:
            gaps.append("%s -> %s (%s) missing from the export — tracked as %s" % (t[0], t[1], t[2], bug))
        else:
            fails.append("%s -> %s (%s) is ALLOWED by ARCHITECTURE.md §3 but missing from the export"
                         % (t[0], t[1], t[2]))
    for t in extra:
        # Never a tracked gap: the implementation permitting a wire the contract does not
        # is a privilege question, not a missing feature.
        fails.append("%s -> %s (%s) is allowed by the export but NOT by ARCHITECTURE.md §3"
                     % (t[0], t[1], t[2]))
    for t in sorted(KNOWN_GAPS):
        if t in sdk:
            fails.append("%s -> %s (%s) is marked %s but is now present — remove it from "
                         "KNOWN_GAPS and close the bug" % (t[0], t[1], t[2], KNOWN_GAPS[t]))

    print("contract (QA, from §3 prose): %d allowed" % len(qa))
    print("export   (wheel-core):        %d allowed" % len(sdk))
    if gaps:
        print("\n%d TRACKED GAP(S):" % len(gaps))
        for g in gaps:
            print("  -", g)
    if fails:
        print("\nwire matrix conformance: %d FAILED" % len(fails))
        for f in fails:
            print("  -", f)
        return 1
    print("\nwire matrix conformance: export matches the contract (%d tracked gap(s))" % len(gaps))
    return 0

if __name__ == "__main__":
    sys.exit(main())
